//! The message archive: every message Hugh sees in its guild, the text each
//! edit replaced, and when it was deleted, in one database file. Staff read it as
//! evidence and the trainer reads it as history.
//!
//! One writer thread owns the database. The gateway hands it work through a
//! bounded queue and never waits on the disk, and because edits and deletions
//! travel down the same queue as inserts, a deletion can never overtake the
//! message it deletes.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard,
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use tokio::sync::oneshot;
use tracing::{error, info, warn};

use crate::{config::ArchiveConfig, features};

/// Work waiting for the writer. Past this the gateway drops archive writes
/// rather than wait for the disk, and says so in the log.
const QUEUE_DEPTH: usize = 8_192;
/// Most operations committed in one transaction.
const BATCH: usize = 512;
/// How often the writer applies retention and the size ceiling.
const MAINTENANCE_INTERVAL: Duration = Duration::from_hours(1);
/// What a row costs beyond its text: IDs, timestamps, features and its index
/// entries. The size ceiling counts this for every row it cannot thin.
const ROW_OVERHEAD: i64 = 128;
/// Authors whose last message time is remembered for the gap feature before
/// the oldest are forgotten.
const SEEN_LIMIT: usize = 100_000;
/// Milliseconds between the Unix epoch and Discord's, the zero of a snowflake.
const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;
const SCHEMA_VERSION: i64 = 2;
/// Messages whose text is kept whatever retention and the ceiling say: what
/// automod removed, what a case refers to, and what staff gave a verdict on,
/// which is the most valuable training data the archive holds.
const UNREFERENCED: &str =
    "flagged = 0 AND case_id IS NULL AND id NOT IN (SELECT message_id FROM feedback)";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    channel_id INTEGER NOT NULL,
    author_id INTEGER NOT NULL,
    reply_to INTEGER,
    created_at INTEGER NOT NULL,
    content TEXT,
    attachments TEXT,
    features BLOB NOT NULL,
    feature_version INTEGER NOT NULL,
    edited_at INTEGER,
    deleted_at INTEGER,
    flagged INTEGER NOT NULL DEFAULT 0,
    case_id INTEGER
);
CREATE INDEX IF NOT EXISTS messages_by_author ON messages (author_id, created_at);
CREATE INDEX IF NOT EXISTS messages_by_channel ON messages (channel_id, created_at);
CREATE TABLE IF NOT EXISTS revisions (
    message_id INTEGER NOT NULL,
    replaced_at INTEGER NOT NULL,
    content TEXT
);
CREATE INDEX IF NOT EXISTS revisions_by_message ON revisions (message_id, replaced_at);
CREATE TABLE IF NOT EXISTS feedback (
    message_id INTEGER NOT NULL,
    moderator_id INTEGER NOT NULL,
    verdict INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (message_id, moderator_id)
);
CREATE TABLE IF NOT EXISTS scores (
    message_id INTEGER PRIMARY KEY,
    model TEXT NOT NULL,
    actionable REAL NOT NULL,
    probabilities TEXT NOT NULL,
    anomaly REAL,
    scored_at INTEGER NOT NULL
);
";

/// A message on its way into the archive. Times are Unix milliseconds.
#[derive(Debug, Clone)]
pub struct ArchivedMessage {
    pub id: u64,
    pub channel_id: u64,
    pub author_id: u64,
    pub reply_to: Option<u64>,
    pub created_at: i64,
    pub content: String,
    /// File names only: Discord's attachment links are signed and expire.
    pub attachments: Vec<String>,
    pub features: [u8; features::COUNT],
}

/// A message as the archive held it, handed back when it is deleted.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: u64,
    pub channel_id: u64,
    pub author_id: u64,
    pub created_at: i64,
    /// `None` once retention or the size ceiling has dropped the text.
    pub content: Option<String>,
    pub attachments: Vec<String>,
    /// Hugh's automod removed it.
    pub flagged: bool,
    pub case_id: Option<u64>,
}

/// What Hugh Mind made of a message.
#[derive(Debug, Clone)]
pub struct ScoreRow {
    pub message_id: u64,
    pub model: String,
    pub actionable: f64,
    /// Every label's probability, as JSON.
    pub probabilities: String,
    pub anomaly: Option<f64>,
    pub scored_at: i64,
}

#[derive(Debug)]
enum Op {
    Score(ScoreRow),
    Feedback {
        message_id: u64,
        moderator_id: u64,
        verdict: bool,
        at: i64,
    },
    Insert(ArchivedMessage),
    Edit {
        message: ArchivedMessage,
        edited_at: i64,
        reply: oneshot::Sender<Option<String>>,
    },
    Delete {
        ids: Vec<u64>,
        deleted_at: i64,
        reply: oneshot::Sender<Vec<StoredMessage>>,
    },
    Flag {
        id: u64,
        case_id: Option<u64>,
    },
}

/// The handle the gateway holds. Cheap to clone: every clone feeds the same
/// writer.
#[derive(Debug, Clone)]
pub struct Archive {
    sender: SyncSender<Op>,
    /// When each author last posted, for the gap feature.
    last_seen: Arc<Mutex<HashMap<u64, i64>>>,
}

impl Archive {
    /// Opens or creates the archive and starts its writer. Failing here stops
    /// Hugh starting, the same as an unreadable moderation record does.
    pub fn open(config: &ArchiveConfig) -> Result<Self> {
        let path = Path::new(&config.path);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create archive directory {}", parent.display())
            })?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open the archive at {}", path.display()))?;
        prepare(&connection).with_context(|| format!("failed to prepare {}", path.display()))?;

        let writer = Writer::new(connection, config);
        let (sender, receiver) = mpsc::sync_channel(QUEUE_DEPTH);
        thread::Builder::new()
            .name("hugh-archive".to_owned())
            .spawn(move || writer.run(&receiver))
            .context("failed to start the archive writer")?;
        Ok(Self {
            sender,
            last_seen: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Seconds since this author's previous message, remembering this one.
    pub fn gap(&self, author_id: u64, at: i64) -> Option<f64> {
        let previous = {
            let mut seen = lock(&self.last_seen);
            if seen.len() >= SEEN_LIMIT {
                let cutoff = at.saturating_sub(7 * 86_400_000);
                seen.retain(|_, last| *last >= cutoff);
            }
            seen.insert(author_id, at)
        }?;
        #[allow(clippy::cast_precision_loss)]
        Some(at.saturating_sub(previous).max(0) as f64 / 1_000.0)
    }

    pub fn record(&self, message: ArchivedMessage) {
        self.send(Op::Insert(message));
    }

    /// Files an edit. Returns the text it replaced, when the archive held the
    /// message and the text really changed; Discord also sends an edit when
    /// it does no more than unfurl a link.
    pub async fn edit(&self, message: ArchivedMessage, edited_at: i64) -> Option<String> {
        let (reply, answer) = oneshot::channel();
        if !self.send(Op::Edit {
            message,
            edited_at,
            reply,
        }) {
            return None;
        }
        answer.await.ok().flatten()
    }

    /// Marks messages deleted and returns what the archive held for each one
    /// it knew about and had not already seen deleted.
    pub async fn delete(&self, ids: Vec<u64>, deleted_at: i64) -> Vec<StoredMessage> {
        let (reply, answer) = oneshot::channel();
        if !self.send(Op::Delete {
            ids,
            deleted_at,
            reply,
        }) {
            return Vec::new();
        }
        answer.await.unwrap_or_default()
    }

    /// Marks a message as one Hugh's automod removed, and once the case is
    /// numbered, which case. A flagged message's text is evidence, so
    /// retention and the size ceiling never drop it.
    pub fn flag(&self, id: u64, case_id: Option<u64>) {
        self.send(Op::Flag { id, case_id });
    }

    pub fn score(&self, row: ScoreRow) {
        self.send(Op::Score(row));
    }

    /// A staff verdict on a message: `true` needed action, `false` was fine.
    /// One verdict per moderator per message; pressing again changes it.
    pub fn feedback(&self, message_id: u64, moderator_id: u64, verdict: bool, at: i64) {
        self.send(Op::Feedback {
            message_id,
            moderator_id,
            verdict,
            at,
        });
    }

    fn send(&self, op: Op) -> bool {
        match self.sender.try_send(op) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                warn!("the message archive is behind; a write was dropped");
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                error!("the message archive writer has stopped; a write was dropped");
                false
            }
        }
    }
}

/// The Unix millisecond a Discord ID was minted at, which is when the message
/// was sent, without trusting a client-side timestamp.
#[must_use]
pub fn snowflake_millis(id: u64) -> i64 {
    i64::try_from((id >> 22) + DISCORD_EPOCH_MS).unwrap_or(i64::MAX)
}

fn prepare(connection: &Connection) -> Result<()> {
    // WAL lets the trainer and staff lookups read while the writer writes.
    let mode: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        warn!(mode, "the archive could not switch to WAL journaling");
    }
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("the archive was written by a newer Hugh (schema {version})");
    }
    connection.execute_batch(SCHEMA)?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

struct Writer {
    connection: Connection,
    retention_days: u32,
    max_bytes: i64,
    last_maintenance: Option<Instant>,
}

impl Writer {
    fn new(connection: Connection, config: &ArchiveConfig) -> Self {
        #[allow(clippy::cast_possible_truncation)]
        let max_bytes = (config.max_gigabytes * 1e9).min(9e18) as i64;
        Self {
            connection,
            retention_days: config.retention_days,
            max_bytes,
            last_maintenance: None,
        }
    }

    fn run(mut self, receiver: &Receiver<Op>) {
        self.maintain();
        loop {
            let first = match receiver.recv_timeout(MAINTENANCE_INTERVAL) {
                Ok(op) => op,
                Err(RecvTimeoutError::Timeout) => {
                    self.maintain();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let mut batch = vec![first];
            batch.extend(receiver.try_iter().take(BATCH - 1));
            if let Err(error) = self.apply(batch) {
                error!(?error, "failed to write to the message archive");
            }
            // A busy server never lets the receive time out, so maintenance
            // also runs once its interval has passed between batches.
            if self
                .last_maintenance
                .is_none_or(|at| at.elapsed() >= MAINTENANCE_INTERVAL)
            {
                self.maintain();
            }
        }
    }

    fn apply(&mut self, batch: Vec<Op>) -> Result<()> {
        let mut edits = Vec::new();
        let mut deletions = Vec::new();
        let transaction = self.connection.transaction()?;
        for op in batch {
            match op {
                Op::Insert(message) => insert(&transaction, &message)?,
                Op::Edit {
                    message,
                    edited_at,
                    reply,
                } => edits.push((reply, edit(&transaction, &message, edited_at)?)),
                Op::Delete {
                    ids,
                    deleted_at,
                    reply,
                } => deletions.push((reply, delete(&transaction, &ids, deleted_at)?)),
                Op::Flag { id, case_id } => {
                    transaction.execute(
                        "UPDATE messages SET flagged = 1, case_id = COALESCE(?2, case_id) \
                         WHERE id = ?1",
                        params![sql_id(id), case_id.map(sql_id)],
                    )?;
                }
                Op::Score(row) => {
                    transaction.execute(
                        "INSERT OR REPLACE INTO scores \
                         (message_id, model, actionable, probabilities, anomaly, scored_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            sql_id(row.message_id),
                            row.model,
                            row.actionable,
                            row.probabilities,
                            row.anomaly,
                            row.scored_at,
                        ],
                    )?;
                }
                Op::Feedback {
                    message_id,
                    moderator_id,
                    verdict,
                    at,
                } => {
                    transaction.execute(
                        "INSERT OR REPLACE INTO feedback (message_id, moderator_id, verdict, at) \
                         VALUES (?1, ?2, ?3, ?4)",
                        params![sql_id(message_id), sql_id(moderator_id), verdict, at],
                    )?;
                }
            }
        }
        transaction.commit()?;
        // Answers wait for the commit, so nobody is told about a change that
        // never landed. A caller that stopped waiting is not an error.
        for (reply, previous) in edits {
            let _ = reply.send(previous);
        }
        for (reply, rows) in deletions {
            let _ = reply.send(rows);
        }
        Ok(())
    }

    fn maintain(&mut self) {
        self.last_maintenance = Some(Instant::now());
        if let Err(error) = self.apply_retention(Utc::now().timestamp_millis()) {
            error!(?error, "failed to apply archive retention");
        }
        if let Err(error) = self.enforce_ceiling() {
            error!(?error, "failed to hold the archive under its size ceiling");
        }
    }

    /// Drops the text of messages older than the retention period, unless a
    /// case refers to them.
    fn apply_retention(&self, now: i64) -> Result<()> {
        if self.retention_days == 0 {
            return Ok(());
        }
        let cutoff = now.saturating_sub(i64::from(self.retention_days) * 86_400_000);
        let cleared = self.connection.execute(
            &format!(
                "UPDATE messages SET content = NULL, attachments = NULL \
                 WHERE created_at < ?1 AND content IS NOT NULL AND {UNREFERENCED}"
            ),
            [cutoff],
        )?;
        let revisions = self.connection.execute(
            &format!(
                "DELETE FROM revisions WHERE replaced_at < ?1 AND message_id IN \
                 (SELECT id FROM messages WHERE {UNREFERENCED})"
            ),
            [cutoff],
        )?;
        if cleared + revisions > 0 {
            info!(
                cleared,
                revisions, "archive retention dropped old message text"
            );
        }
        Ok(())
    }

    /// Keeps the archive under `max_gigabytes` by dropping the oldest text no
    /// case refers to. The size is estimated from what the rows hold rather
    /// than read off the file: the database reuses freed space instead of shrinking
    /// the file, so the file alone would never show the thinning worked.
    fn enforce_ceiling(&self) -> Result<()> {
        let estimated: i64 = self.connection.query_row(
            "SELECT \
               (SELECT COALESCE(SUM(COALESCE(LENGTH(CAST(content AS BLOB)), 0) \
                                    + COALESCE(LENGTH(attachments), 0)), 0) \
                       + COUNT(*) * ?1 FROM messages) \
             + (SELECT COALESCE(SUM(LENGTH(CAST(content AS BLOB))), 0) FROM revisions)",
            [ROW_OVERHEAD],
            |row| row.get(0),
        )?;
        let overage = estimated.saturating_sub(self.max_bytes);
        if overage <= 0 {
            return Ok(());
        }
        // Oldest first, a row is thinned while the rows before it have not yet
        // covered the overage, so one pass lands just under the ceiling.
        let thinned = self.connection.execute(
            &format!(
                "UPDATE messages SET content = NULL, attachments = NULL WHERE id IN ( \
                   SELECT id FROM ( \
                     SELECT id, size, SUM(size) OVER (ORDER BY created_at, id) AS running \
                     FROM ( \
                       SELECT id, created_at, \
                              COALESCE(LENGTH(CAST(content AS BLOB)), 0) \
                                + COALESCE(LENGTH(attachments), 0) AS size \
                       FROM messages \
                       WHERE content IS NOT NULL AND {UNREFERENCED})) \
                   WHERE running - size < ?1)"
            ),
            [overage],
        )?;
        if thinned == 0 {
            warn!(
                estimated_bytes = estimated,
                "the archive is over archive.max_gigabytes and holds nothing but evidence; raise \
                 the ceiling"
            );
        } else {
            warn!(
                thinned,
                "the archive reached archive.max_gigabytes; the oldest unreferenced text was dropped"
            );
        }
        Ok(())
    }
}

fn insert(transaction: &Transaction<'_>, message: &ArchivedMessage) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO messages \
         (id, channel_id, author_id, reply_to, created_at, content, attachments, features, \
          feature_version) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            sql_id(message.id),
            sql_id(message.channel_id),
            sql_id(message.author_id),
            message.reply_to.map(sql_id),
            message.created_at,
            message.content,
            attachment_list(&message.attachments)?,
            &message.features[..],
            features::VERSION,
        ],
    )?;
    Ok(())
}

/// Applies an edit and returns the text it replaced. A message the archive
/// never saw, because it predates Hugh, is filed as it now stands.
fn edit(
    transaction: &Transaction<'_>,
    message: &ArchivedMessage,
    edited_at: i64,
) -> Result<Option<String>> {
    let current: Option<Option<String>> = transaction
        .query_row(
            "SELECT content FROM messages WHERE id = ?1",
            [sql_id(message.id)],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current) = current else {
        insert(transaction, message)?;
        return Ok(None);
    };
    if current.as_deref() == Some(message.content.as_str()) {
        return Ok(None);
    }
    transaction.execute(
        "INSERT INTO revisions (message_id, replaced_at, content) VALUES (?1, ?2, ?3)",
        params![sql_id(message.id), edited_at, current],
    )?;
    transaction.execute(
        "UPDATE messages SET content = ?2, attachments = ?3, edited_at = ?4 WHERE id = ?1",
        params![
            sql_id(message.id),
            message.content,
            attachment_list(&message.attachments)?,
            edited_at,
        ],
    )?;
    Ok(current)
}

fn delete(
    transaction: &Transaction<'_>,
    ids: &[u64],
    deleted_at: i64,
) -> Result<Vec<StoredMessage>> {
    let mut select = transaction.prepare_cached(
        "SELECT id, channel_id, author_id, created_at, content, attachments, flagged, case_id \
         FROM messages WHERE id = ?1 AND deleted_at IS NULL",
    )?;
    let mut mark =
        transaction.prepare_cached("UPDATE messages SET deleted_at = ?2 WHERE id = ?1")?;
    let mut rows = Vec::new();
    for id in ids {
        if let Some(row) = select.query_row([sql_id(*id)], stored).optional()? {
            mark.execute(params![sql_id(*id), deleted_at])?;
            rows.push(row);
        }
    }
    Ok(rows)
}

fn stored(row: &Row<'_>) -> rusqlite::Result<StoredMessage> {
    let attachments: Option<String> = row.get(5)?;
    Ok(StoredMessage {
        id: from_sql(row.get(0)?),
        channel_id: from_sql(row.get(1)?),
        author_id: from_sql(row.get(2)?),
        created_at: row.get(3)?,
        content: row.get(4)?,
        attachments: attachments
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default(),
        flagged: row.get(6)?,
        case_id: row.get::<_, Option<i64>>(7)?.map(from_sql),
    })
}

fn attachment_list(names: &[String]) -> Result<Option<String>> {
    if names.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(names)?))
}

/// Database integers are signed. Discord IDs stay below 2^63 for centuries yet,
/// so the conversion is lossless in practice and saturates if it ever is not.
fn sql_id(id: u64) -> i64 {
    i64::try_from(id).unwrap_or(i64::MAX)
}

fn from_sql(id: i64) -> u64 {
    u64::try_from(id).unwrap_or_default()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(directory: &tempfile::TempDir) -> ArchiveConfig {
        ArchiveConfig {
            enabled: true,
            path: directory
                .path()
                .join("archive.sqlite3")
                .to_string_lossy()
                .into_owned(),
            ..ArchiveConfig::default()
        }
    }

    fn message(id: u64, created_at: i64, content: &str) -> ArchivedMessage {
        ArchivedMessage {
            id,
            channel_id: 10,
            author_id: 20,
            reply_to: None,
            created_at,
            content: content.to_owned(),
            attachments: Vec::new(),
            features: [0; features::COUNT],
        }
    }

    fn writer(directory: &tempfile::TempDir, max_gigabytes: f64, retention_days: u32) -> Writer {
        let config = ArchiveConfig {
            retention_days,
            max_gigabytes,
            ..config(directory)
        };
        let connection = Connection::open(&config.path).unwrap();
        prepare(&connection).unwrap();
        Writer::new(connection, &config)
    }

    fn content(writer: &Writer, id: u64) -> Option<String> {
        writer
            .connection
            .query_row(
                "SELECT content FROM messages WHERE id = ?1",
                [sql_id(id)],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[tokio::test]
    async fn a_deleted_message_comes_back_with_its_text_once() {
        let directory = tempfile::tempdir().unwrap();
        let archive = Archive::open(&config(&directory)).unwrap();
        let mut sent = message(1, 1_000, "hello there");
        sent.attachments = vec!["cat.png".to_owned()];
        archive.record(sent);

        let removed = archive.delete(vec![1, 2], 5_000).await;
        assert_eq!(removed.len(), 1, "an unknown ID is simply skipped");
        assert_eq!(removed[0].content.as_deref(), Some("hello there"));
        assert_eq!(removed[0].attachments, ["cat.png"]);
        assert!(!removed[0].flagged);
        assert!(archive.delete(vec![1], 6_000).await.is_empty());
    }

    #[tokio::test]
    async fn an_edit_returns_the_text_it_replaced_only_when_it_changed() {
        let directory = tempfile::tempdir().unwrap();
        let archive = Archive::open(&config(&directory)).unwrap();
        archive.record(message(1, 1_000, "frist"));

        assert_eq!(
            archive.edit(message(1, 1_000, "first"), 2_000).await,
            Some("frist".to_owned())
        );
        // A link unfurling sends an edit with the same text.
        assert_eq!(archive.edit(message(1, 1_000, "first"), 3_000).await, None);
        // A message from before Hugh started is filed as it now stands.
        assert_eq!(archive.edit(message(9, 1_000, "old"), 3_000).await, None);
        let removed = archive.delete(vec![1, 9], 4_000).await;
        let texts: Vec<_> = removed.iter().map(|row| row.content.as_deref()).collect();
        assert_eq!(texts, [Some("first"), Some("old")]);
    }

    #[tokio::test]
    async fn automod_removals_carry_their_case() {
        let directory = tempfile::tempdir().unwrap();
        let archive = Archive::open(&config(&directory)).unwrap();
        archive.record(message(1, 1_000, "spam"));
        archive.flag(1, None);
        archive.flag(1, Some(42));
        // The case number is never wiped by a later flag that lacks one.
        archive.flag(1, None);

        let removed = archive.delete(vec![1], 2_000).await;
        assert!(removed[0].flagged);
        assert_eq!(removed[0].case_id, Some(42));
    }

    #[test]
    fn the_gap_is_the_time_since_the_same_authors_last_message() {
        let directory = tempfile::tempdir().unwrap();
        let archive = Archive::open(&config(&directory)).unwrap();
        assert_eq!(archive.gap(1, 10_000), None);
        assert_eq!(archive.gap(2, 11_000), None);
        assert_eq!(archive.gap(1, 13_500), Some(3.5));
    }

    #[test]
    fn snowflakes_carry_their_creation_time() {
        // Discord's documented example: 175928847299117063 was minted at
        // 2016-04-30 11:18:25.796 UTC.
        assert_eq!(snowflake_millis(175_928_847_299_117_063), 1_462_015_105_796);
    }

    #[test]
    fn retention_drops_old_text_but_keeps_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let mut writer = writer(&directory, 30.0, 30);
        let day = 86_400_000;
        let now = 100 * day;
        let mut batch = vec![
            Op::Insert(message(1, now - 40 * day, "old chatter")),
            Op::Insert(message(2, now - 40 * day, "old scam")),
            Op::Insert(message(3, now - day, "recent")),
            Op::Insert(message(4, now - 40 * day, "old but judged")),
        ];
        batch.push(Op::Flag {
            id: 2,
            case_id: Some(7),
        });
        // A staff verdict is a training label, so its text is kept too.
        batch.push(Op::Feedback {
            message_id: 4,
            moderator_id: 9,
            verdict: false,
            at: now,
        });
        writer.apply(batch).unwrap();

        writer.apply_retention(now).unwrap();
        assert_eq!(content(&writer, 1), None);
        assert_eq!(content(&writer, 2).as_deref(), Some("old scam"));
        assert_eq!(content(&writer, 3).as_deref(), Some("recent"));
        assert_eq!(content(&writer, 4).as_deref(), Some("old but judged"));
    }

    #[test]
    fn the_ceiling_thins_the_oldest_unreferenced_text_first() {
        let directory = tempfile::tempdir().unwrap();
        // Four rows of overhead plus 300 bytes of text, against a ceiling
        // with room for the overhead and two of the three texts.
        let ceiling = 4 * ROW_OVERHEAD + 200;
        #[allow(clippy::cast_precision_loss)]
        let mut writer = writer(&directory, ceiling as f64 / 1e9, 0);
        let text = "x".repeat(100);
        writer
            .apply(vec![
                Op::Insert(message(1, 1, &text)),
                Op::Insert(message(2, 2, &text)),
                Op::Insert(message(3, 3, &text)),
                Op::Insert(message(4, 4, "")),
                Op::Flag {
                    id: 1,
                    case_id: None,
                },
            ])
            .unwrap();

        writer.enforce_ceiling().unwrap();
        assert_eq!(content(&writer, 1).as_deref(), Some(text.as_str()));
        assert_eq!(content(&writer, 2), None);
        assert_eq!(content(&writer, 3).as_deref(), Some(text.as_str()));
        // Back under the ceiling, a second pass leaves everything alone.
        writer.enforce_ceiling().unwrap();
        assert_eq!(content(&writer, 3).as_deref(), Some(text.as_str()));
    }
}
