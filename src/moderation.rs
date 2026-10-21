//! The moderation record: a durable, append-only history of every punishment
//! Hugh or a moderator applied, the standing that history adds up to, and the
//! sweep that lifts a temporary ban once it has run out.
//!
//! Case numbers come from the incident log, so one number identifies both the
//! card in the mod log and the row in this record.

use std::{cmp::Reverse, collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serenity::all::{GuildId, Http, UserId};
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt,
    sync::Mutex,
};
use tracing::{error, info};

use crate::{
    config::{Config, PunishmentWeights, StandingBand, Strictness},
    incident::{Incident, IncidentReporter},
};

/// Every kind of punishment the record can hold. `Automod` covers one enforced
/// message, whatever mix of guards it tripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Punishment {
    Warn,
    Timeout,
    Kick,
    Ban,
    TemporaryBan,
    Quarantine,
    Automod,
}

impl Punishment {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Timeout => "timeout",
            Self::Kick => "kick",
            Self::Ban => "ban",
            Self::TemporaryBan => "temporary_ban",
            Self::Quarantine => "quarantine",
            Self::Automod => "automod",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Warn => "Warning",
            Self::Timeout => "Timeout",
            Self::Kick => "Kick",
            Self::Ban => "Ban",
            Self::TemporaryBan => "Temporary ban",
            Self::Quarantine => "Quarantine",
            Self::Automod => "Automod",
        }
    }

    /// Icon slot used for this punishment on cards.
    #[must_use]
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Timeout => "timeout",
            Self::Kick => "kicked",
            Self::Ban | Self::TemporaryBan => "ban",
            Self::Quarantine => "quarantine",
            Self::Automod => "deleted",
        }
    }

    /// The standing cost configured for this punishment.
    #[must_use]
    pub const fn weight(self, weights: &PunishmentWeights) -> f64 {
        match self {
            Self::Warn => weights.warn,
            Self::Timeout => weights.timeout,
            Self::Kick => weights.kick,
            Self::Ban => weights.ban,
            Self::TemporaryBan => weights.temporary_ban,
            Self::Quarantine => weights.quarantine,
            Self::Automod => weights.automod,
        }
    }
}

/// A punishment on its way into the record.
#[derive(Debug, Clone)]
pub struct NewPunishment {
    /// The case number the incident log already assigned.
    pub case_id: u64,
    pub user_id: u64,
    pub kind: Punishment,
    pub reason: String,
    pub moderator_id: Option<u64>,
    /// Length of a timeout or temporary ban. `None` is permanent or instant.
    pub duration_seconds: Option<u64>,
    pub weight: f64,
}

/// One row of the record, as Hugh holds it in memory.
#[derive(Debug, Clone)]
pub struct Record {
    pub case_id: u64,
    pub user_id: u64,
    pub kind: Punishment,
    pub reason: String,
    pub moderator_id: Option<u64>,
    pub at: DateTime<Utc>,
    pub duration_seconds: Option<u64>,
    pub weight: f64,
    /// A moderator took this case back; it no longer costs standing.
    pub revoked: bool,
    /// A temporary ban that has already been lifted.
    pub lifted: bool,
}

impl Record {
    /// When a temporary ban runs out.
    #[must_use]
    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        let seconds = self.duration_seconds.filter(|seconds| *seconds > 0)?;
        let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
        Some(self.at + chrono::Duration::seconds(seconds))
    }

    /// The share of this case's weight that still counts, after decay. A
    /// revoked case counts for nothing from the moment it is taken back.
    #[must_use]
    pub fn decayed_weight(&self, decay_days: f64, now: DateTime<Utc>) -> f64 {
        if self.revoked || self.weight <= 0.0 || decay_days <= 0.0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let age_days = (now - self.at).num_seconds() as f64 / 86_400.0;
        let remaining = (1.0 - age_days / decay_days).clamp(0.0, 1.0);
        self.weight * remaining
    }
}

/// What a member's history adds up to.
#[derive(Debug, Clone)]
pub struct Standing {
    pub points: f64,
    pub starting_points: f64,
    pub band: StandingBand,
    pub strictness: Strictness,
    /// Weight still counting against the member, after decay.
    pub active_weight: f64,
    pub counts: Vec<(Punishment, usize)>,
    pub total_cases: usize,
    pub revoked_cases: usize,
    pub latest: Option<Record>,
}

impl Standing {
    /// The standing a member with no history holds.
    #[must_use]
    pub fn clean(config: &Config) -> Self {
        let points = config.standing.starting_points;
        let band = config.standing.band(points);
        Self {
            points,
            starting_points: points,
            band,
            strictness: config.standing.strictness(band),
            active_weight: 0.0,
            counts: Vec::new(),
            total_cases: 0,
            revoked_cases: 0,
            latest: None,
        }
    }

    /// Standing as a share of the starting points, for a progress bar.
    #[must_use]
    pub fn fraction(&self) -> f64 {
        if self.starting_points <= 0.0 {
            return 0.0;
        }
        (self.points / self.starting_points).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredRecord {
    case_id: u64,
    user_id: String,
    kind: Punishment,
    reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    moderator_id: Option<String>,
    at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_seconds: Option<u64>,
    weight: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum StoreEvent {
    Recorded(StoredRecord),
    Revoked {
        case_id: u64,
        at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        moderator_id: Option<String>,
    },
    Lifted {
        case_id: u64,
        at: DateTime<Utc>,
    },
}

#[derive(Debug)]
struct StoreInner {
    /// Keyed by case number, which the incident log keeps ascending.
    records: BTreeMap<u64, Record>,
    /// Case numbers per member, oldest first.
    by_user: BTreeMap<u64, Vec<u64>>,
    file: File,
}

/// The durable moderation record. Cheap to clone: every clone shares one file
/// handle and one in-memory index.
#[derive(Debug, Clone)]
pub struct ModerationStore {
    inner: Arc<Mutex<StoreInner>>,
}

impl ModerationStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create moderation directory {}", parent.display())
            })?;
        }

        let mut records: BTreeMap<u64, Record> = BTreeMap::new();
        match fs::read_to_string(path).await {
            Ok(contents) => {
                for (index, line) in contents.lines().enumerate() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let event: StoreEvent = serde_json::from_str(line).with_context(|| {
                        format!(
                            "invalid moderation record at {}:{}",
                            path.display(),
                            index + 1
                        )
                    })?;
                    apply_event(&mut records, event).with_context(|| {
                        format!("invalid moderation IDs at {}:{}", path.display(), index + 1)
                    })?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        }

        let mut by_user: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
        for record in records.values() {
            by_user
                .entry(record.user_id)
                .or_default()
                .push(record.case_id);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("failed to open moderation record {}", path.display()))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(StoreInner {
                records,
                by_user,
                file,
            })),
        })
    }

    /// Appends a punishment and returns the stored row. The write is flushed
    /// before the row becomes visible, so a crash cannot leave a member
    /// punished with nothing on record.
    pub async fn record(&self, entry: NewPunishment) -> Result<Record> {
        let record = Record {
            case_id: entry.case_id,
            user_id: entry.user_id,
            kind: entry.kind,
            reason: entry.reason,
            moderator_id: entry.moderator_id,
            at: Utc::now(),
            duration_seconds: entry.duration_seconds.filter(|seconds| *seconds > 0),
            weight: entry.weight.max(0.0),
            revoked: false,
            lifted: false,
        };
        let mut inner = self.inner.lock().await;
        append(
            &mut inner.file,
            &StoreEvent::Recorded(StoredRecord {
                case_id: record.case_id,
                user_id: record.user_id.to_string(),
                kind: record.kind,
                reason: record.reason.clone(),
                moderator_id: record.moderator_id.map(|id| id.to_string()),
                at: record.at,
                duration_seconds: record.duration_seconds,
                weight: record.weight,
            }),
        )
        .await?;
        let cases = inner.by_user.entry(record.user_id).or_default();
        if !cases.contains(&record.case_id) {
            cases.push(record.case_id);
        }
        inner.records.insert(record.case_id, record.clone());
        drop(inner);
        Ok(record)
    }

    /// A member's cases, newest first.
    pub async fn history(&self, user_id: u64) -> Vec<Record> {
        let inner = self.inner.lock().await;
        let mut history: Vec<Record> = inner
            .by_user
            .get(&user_id)
            .map(|cases| {
                cases
                    .iter()
                    .filter_map(|case_id| inner.records.get(case_id).cloned())
                    .collect()
            })
            .unwrap_or_default();
        drop(inner);
        history.sort_by_key(|record| Reverse(record.at));
        history
    }

    /// Takes a case back, so it stops costing standing. Returns the row as it
    /// stood before the revocation, or `None` when no such case exists.
    pub async fn revoke(&self, case_id: u64, moderator_id: Option<u64>) -> Result<Option<Record>> {
        let mut inner = self.inner.lock().await;
        let Some(existing) = inner.records.get(&case_id).cloned() else {
            return Ok(None);
        };
        if existing.revoked {
            return Ok(Some(existing));
        }
        append(
            &mut inner.file,
            &StoreEvent::Revoked {
                case_id,
                at: Utc::now(),
                moderator_id: moderator_id.map(|id| id.to_string()),
            },
        )
        .await?;
        if let Some(record) = inner.records.get_mut(&case_id) {
            record.revoked = true;
        }
        drop(inner);
        Ok(Some(existing))
    }

    /// Marks a temporary ban as served. Returns `false` when the case is
    /// unknown or was already lifted.
    pub async fn lift(&self, case_id: u64) -> Result<bool> {
        let mut inner = self.inner.lock().await;
        if inner
            .records
            .get(&case_id)
            .is_none_or(|record| record.lifted)
        {
            return Ok(false);
        }
        append(
            &mut inner.file,
            &StoreEvent::Lifted {
                case_id,
                at: Utc::now(),
            },
        )
        .await?;
        if let Some(record) = inner.records.get_mut(&case_id) {
            record.lifted = true;
        }
        drop(inner);
        Ok(true)
    }

    /// Temporary bans whose time has run out and that nobody has lifted.
    pub async fn due_bans(&self, now: DateTime<Utc>) -> Vec<Record> {
        self.inner
            .lock()
            .await
            .records
            .values()
            .filter(|record| record.kind == Punishment::TemporaryBan)
            .filter(|record| !record.lifted)
            .filter(|record| record.expires_at().is_some_and(|expiry| expiry <= now))
            .cloned()
            .collect()
    }

    /// The member's standing, and the automod strictness that follows from it.
    pub async fn standing(&self, user_id: u64, config: &Config) -> Standing {
        let history = self.history(user_id).await;
        standing_from(&history, config, Utc::now())
    }

    /// The strictness alone, which is all the message path needs.
    pub async fn strictness(&self, user_id: u64, config: &Config) -> Strictness {
        if !config.standing.enabled {
            return Strictness::default();
        }
        self.standing(user_id, config).await.strictness
    }
}

/// Adds a history up into a standing. Separated from the store so the maths is
/// testable without touching a file.
#[must_use]
pub fn standing_from(history: &[Record], config: &Config, now: DateTime<Utc>) -> Standing {
    let standing = &config.standing;
    if history.is_empty() {
        return Standing::clean(config);
    }

    let mut active_weight = 0.0;
    let mut counts: BTreeMap<Punishment, usize> = BTreeMap::new();
    let mut revoked_cases = 0;
    for record in history {
        active_weight += record.decayed_weight(standing.decay_days, now);
        if record.revoked {
            revoked_cases += 1;
        } else {
            *counts.entry(record.kind).or_default() += 1;
        }
    }
    let points = (standing.starting_points - active_weight).clamp(0.0, standing.starting_points);
    let band = standing.band(points);
    Standing {
        points,
        starting_points: standing.starting_points,
        band,
        strictness: standing.strictness(band),
        active_weight,
        counts: counts.into_iter().collect(),
        total_cases: history.len(),
        revoked_cases,
        latest: history.first().cloned(),
    }
}

/// Parses a moderator-written length such as `30m`, `1h30m`, `2d` or `1w`.
/// `None` means permanent: that is what an omitted, empty, `perm`, `permanent`,
/// `forever` or `0` value asks for.
pub fn parse_duration(input: &str) -> Result<Option<u64>> {
    let text = input.trim().to_ascii_lowercase();
    if text.is_empty() || matches!(text.as_str(), "perm" | "permanent" | "forever" | "0") {
        return Ok(None);
    }

    let mut total: u64 = 0;
    let mut digits = String::new();
    let mut matched = false;
    for character in text.chars() {
        match character {
            '0'..='9' => digits.push(character),
            ' ' | ',' => {}
            unit => {
                if digits.is_empty() {
                    bail!("{input:?} is not a length; write it as 30m, 1h30m, 2d or 1w");
                }
                let size = match unit {
                    's' => 1,
                    'm' => 60,
                    'h' => 3_600,
                    'd' => 86_400,
                    'w' => 604_800,
                    other => bail!(
                        "{other:?} is not a length unit; use s for seconds, m for minutes, h for hours, d for days or w for weeks"
                    ),
                };
                let count: u64 = digits
                    .parse()
                    .with_context(|| format!("{digits:?} is too large to be a length"))?;
                total = total
                    .checked_add(count.saturating_mul(size))
                    .context("that length is too long to apply")?;
                digits.clear();
                matched = true;
            }
        }
    }
    if !digits.is_empty() {
        bail!("{input:?} is missing a unit; write it as 30m, 1h30m, 2d or 1w");
    }
    if !matched || total == 0 {
        bail!("{input:?} is not a usable length; write it as 30m, 1h30m, 2d or 1w");
    }
    Ok(Some(total))
}

/// Lifts every temporary ban that has run out, for as long as Hugh is running.
pub async fn sweep_expired_bans(
    http: Arc<Http>,
    store: ModerationStore,
    config: Arc<Config>,
    incidents: IncidentReporter,
) {
    let guild_id = GuildId::new(config.guild_id);
    let interval = Duration::from_secs(config.moderation.expiry_sweep_seconds.max(1));
    loop {
        tokio::time::sleep(interval).await;
        for record in store.due_bans(Utc::now()).await {
            let user_id = UserId::new(record.user_id);
            // The lift is written first either way: a ban Discord has already
            // dropped must not be retried on every sweep from here on.
            let result = guild_id.unban(&http, user_id).await;
            if let Err(error) = store.lift(record.case_id).await {
                error!(
                    ?error,
                    case_id = record.case_id,
                    "failed to mark a temporary ban as served"
                );
                continue;
            }
            match &result {
                Ok(()) => info!(
                    case_id = record.case_id,
                    user_id = record.user_id,
                    "temporary ban expired and was lifted"
                ),
                Err(error) => {
                    error!(
                        ?error,
                        case_id = record.case_id,
                        user_id = record.user_id,
                        "failed to lift an expired temporary ban"
                    );
                }
            }
            let action = if result.is_ok() {
                "ban_expired"
            } else {
                "ban_expiry_failed"
            };
            let mut incident = Incident::new(
                "temporary_ban_expiry",
                config.guild_id,
                action,
                format!("The temporary ban from Case #{} ran out", record.case_id),
            );
            incident.actor_id = Some(record.user_id);
            incident.details.insert(
                "target_user_id".into(),
                serde_json::Value::from(record.user_id),
            );
            incident
                .details
                .insert("original_case".into(), record.case_id.into());
            if let Some(seconds) = record.duration_seconds {
                incident
                    .details
                    .insert("duration_seconds".into(), seconds.into());
            }
            incidents.report(&http, incident).await;
        }
    }
}

async fn append(file: &mut File, event: &StoreEvent) -> Result<()> {
    let mut line = serde_json::to_vec(event).context("failed to serialize moderation record")?;
    line.push(b'\n');
    file.write_all(&line)
        .await
        .context("failed to write moderation record")?;
    file.flush()
        .await
        .context("failed to flush moderation record")
}

fn apply_event(records: &mut BTreeMap<u64, Record>, event: StoreEvent) -> Result<()> {
    match event {
        StoreEvent::Recorded(stored) => {
            let record = Record {
                case_id: stored.case_id,
                user_id: stored.user_id.parse().context("invalid user ID")?,
                kind: stored.kind,
                reason: stored.reason,
                moderator_id: stored
                    .moderator_id
                    .map(|id| id.parse().context("invalid moderator ID"))
                    .transpose()?,
                at: stored.at,
                duration_seconds: stored.duration_seconds,
                weight: stored.weight,
                revoked: false,
                lifted: false,
            };
            records.insert(record.case_id, record);
        }
        StoreEvent::Revoked { case_id, .. } => {
            if let Some(record) = records.get_mut(&case_id) {
                record.revoked = true;
            }
        }
        StoreEvent::Lifted { case_id, .. } => {
            if let Some(record) = records.get_mut(&case_id) {
                record.lifted = true;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        let config: Config = toml::from_str(
            "guild_id = \"1\"\nincident_channel_id = \"2\"\n\n[role_guard]\nenabled = false",
        )
        .unwrap();
        config.validate().unwrap();
        config
    }

    fn record(case_id: u64, kind: Punishment, weight: f64, days_ago: i64) -> Record {
        Record {
            case_id,
            user_id: 7,
            kind,
            reason: "test".to_owned(),
            moderator_id: Some(9),
            at: Utc::now() - chrono::Duration::days(days_ago),
            duration_seconds: None,
            weight,
            revoked: false,
            lifted: false,
        }
    }

    #[test]
    fn lengths_parse_and_reject_nonsense() {
        assert_eq!(parse_duration("30m").unwrap(), Some(1_800));
        assert_eq!(parse_duration("1h30m").unwrap(), Some(5_400));
        assert_eq!(parse_duration(" 2d ").unwrap(), Some(172_800));
        assert_eq!(parse_duration("1w").unwrap(), Some(604_800));
        assert_eq!(parse_duration("perm").unwrap(), None);
        assert_eq!(parse_duration("").unwrap(), None);
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("5y").is_err());
    }

    #[test]
    fn standing_falls_with_weight_and_recovers_with_age() {
        let config = config();
        let fresh = standing_from(&[record(1, Punishment::Kick, 25.0, 0)], &config, Utc::now());
        assert!((fresh.points - 75.0).abs() < 0.01);
        assert_eq!(fresh.band, StandingBand::Neutral);
        // Neutral is the baseline every limit is written against.
        assert!((fresh.strictness.thresholds - 1.0).abs() < f64::EPSILON);
        assert!((fresh.strictness.timeouts - 1.0).abs() < f64::EPSILON);

        // Below it, worse standing means tighter limits and longer timeouts.
        let poor = standing_from(
            &[
                record(1, Punishment::Kick, 25.0, 0),
                record(2, Punishment::Kick, 25.0, 0),
            ],
            &config,
            Utc::now(),
        );
        assert_eq!(poor.band, StandingBand::Poor);
        assert!(poor.strictness.thresholds < 1.0);
        assert!(poor.strictness.timeouts > 1.0);

        // Half the decay horizon has passed, so half the weight remains.
        let half_decayed = standing_from(
            &[record(1, Punishment::Kick, 25.0, 45)],
            &config,
            Utc::now(),
        );
        assert!((half_decayed.points - 87.5).abs() < 0.2);
        assert_eq!(half_decayed.band, StandingBand::Good);

        // Past the horizon the case is only history.
        let expired = standing_from(
            &[record(1, Punishment::Kick, 25.0, 120)],
            &config,
            Utc::now(),
        );
        assert!((expired.points - 100.0).abs() < 0.01);
        assert_eq!(expired.band, StandingBand::Exemplary);
    }

    #[test]
    fn standing_bottoms_out_rather_than_going_negative() {
        let config = config();
        let history: Vec<Record> = (1..=6)
            .map(|case| record(case, Punishment::Ban, 60.0, 0))
            .collect();
        let standing = standing_from(&history, &config, Utc::now());
        assert!((standing.points - 0.0).abs() < f64::EPSILON);
        assert_eq!(standing.band, StandingBand::Bad);
        assert_eq!(standing.total_cases, 6);
    }

    #[test]
    fn revoked_cases_stop_costing_standing() {
        let config = config();
        let mut revoked = record(1, Punishment::Warn, 8.0, 0);
        revoked.revoked = true;
        let standing = standing_from(&[revoked], &config, Utc::now());
        assert!((standing.points - 100.0).abs() < f64::EPSILON);
        assert_eq!(standing.revoked_cases, 1);
        assert!(standing.counts.is_empty());
    }

    #[tokio::test]
    async fn records_survive_restart_and_revocation_is_durable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("moderation.jsonl");
        let store = ModerationStore::open(&path).await.unwrap();
        store
            .record(NewPunishment {
                case_id: 11,
                user_id: 7,
                kind: Punishment::Warn,
                reason: "first".to_owned(),
                moderator_id: Some(9),
                duration_seconds: None,
                weight: 8.0,
            })
            .await
            .unwrap();
        store
            .record(NewPunishment {
                case_id: 12,
                user_id: 7,
                kind: Punishment::TemporaryBan,
                reason: "second".to_owned(),
                moderator_id: Some(9),
                duration_seconds: Some(1),
                weight: 40.0,
            })
            .await
            .unwrap();
        assert!(store.revoke(11, Some(9)).await.unwrap().is_some());
        drop(store);

        let reopened = ModerationStore::open(&path).await.unwrap();
        let history = reopened.history(7).await;
        assert_eq!(history.len(), 2);
        let revoked = history
            .iter()
            .find(|record| record.case_id == 11)
            .expect("the revoked case is still on the record");
        assert!(revoked.revoked);

        // The temporary ban is due a second later, and only once.
        let due = reopened
            .due_bans(Utc::now() + chrono::Duration::seconds(5))
            .await;
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].case_id, 12);
        assert!(reopened.lift(12).await.unwrap());
        assert!(!reopened.lift(12).await.unwrap());
        assert!(
            reopened
                .due_bans(Utc::now() + chrono::Duration::seconds(5))
                .await
                .is_empty()
        );
        assert!(reopened.revoke(99, None).await.unwrap().is_none());
    }
}
