use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt,
    sync::Mutex,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum StoreEvent {
    Snapshot { user_id: String, role_ids: Vec<String> },
    Cleared { user_id: String },
}

#[derive(Debug)]
struct StoreInner {
    records: BTreeMap<u64, Vec<u64>>,
    file: File,
}

#[derive(Debug, Clone)]
pub struct QuarantineStore {
    inner: Arc<Mutex<StoreInner>>,
}

impl QuarantineStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create quarantine directory {}", parent.display())
            })?;
        }

        let mut records = BTreeMap::new();
        match fs::read_to_string(path).await {
            Ok(contents) => {
                for (index, line) in contents.lines().enumerate() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let event: StoreEvent = serde_json::from_str(line).with_context(|| {
                        format!(
                            "invalid quarantine record at {}:{}",
                            path.display(),
                            index + 1
                        )
                    })?;
                    apply_event(&mut records, event).with_context(|| {
                        format!(
                            "invalid quarantine IDs at {}:{}",
                            path.display(),
                            index + 1
                        )
                    })?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", path.display()));
            }
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("failed to open quarantine store {}", path.display()))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(StoreInner { records, file })),
        })
    }

    /// Saves the original roles exactly once. Returns `false` when a snapshot
    /// already exists, preventing a repeated banish from overwriting recovery data.
    pub async fn snapshot(&self, user_id: u64, role_ids: Vec<u64>) -> Result<bool> {
        let mut inner = self.inner.lock().await;
        if inner.records.contains_key(&user_id) {
            return Ok(false);
        }
        let event = StoreEvent::Snapshot {
            user_id: user_id.to_string(),
            role_ids: role_ids.iter().map(u64::to_string).collect(),
        };
        append(&mut inner.file, &event).await?;
        inner.records.insert(user_id, role_ids);
        Ok(true)
    }

    pub async fn get(&self, user_id: u64) -> Option<Vec<u64>> {
        self.inner.lock().await.records.get(&user_id).cloned()
    }

    /// Clears a snapshot only after the durable `cleared` event is flushed.
    pub async fn clear(&self, user_id: u64) -> Result<bool> {
        let mut inner = self.inner.lock().await;
        if !inner.records.contains_key(&user_id) {
            return Ok(false);
        }
        let event = StoreEvent::Cleared { user_id: user_id.to_string() };
        append(&mut inner.file, &event).await?;
        inner.records.remove(&user_id);
        Ok(true)
    }
}

async fn append(file: &mut File, event: &StoreEvent) -> Result<()> {
    let mut line = serde_json::to_vec(event).context("failed to serialize quarantine record")?;
    line.push(b'\n');
    file.write_all(&line)
        .await
        .context("failed to write quarantine record")?;
    file.flush()
        .await
        .context("failed to flush quarantine record")
}

fn apply_event(records: &mut BTreeMap<u64, Vec<u64>>, event: StoreEvent) -> Result<()> {
    match event {
        StoreEvent::Snapshot { user_id, role_ids } => {
            records.insert(
                user_id.parse().context("invalid user ID")?,
                role_ids
                    .into_iter()
                    .map(|role| role.parse().context("invalid role ID"))
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        StoreEvent::Cleared { user_id } => {
            records.remove(&user_id.parse().context("invalid user ID")?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshots_survive_restart_and_are_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("quarantine.jsonl");
        let store = QuarantineStore::open(&path).await.unwrap();
        assert!(store.snapshot(7, vec![10, 11]).await.unwrap());
        assert!(!store.snapshot(7, vec![99]).await.unwrap());
        drop(store);

        let reopened = QuarantineStore::open(&path).await.unwrap();
        assert_eq!(reopened.get(7).await, Some(vec![10, 11]));
        assert!(reopened.clear(7).await.unwrap());
        assert_eq!(reopened.get(7).await, None);
    }
}
