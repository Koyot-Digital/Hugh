use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt,
    sync::Mutex,
};

#[derive(Debug, Serialize)]
pub struct Incident {
    pub timestamp: DateTime<Utc>,
    pub kind: String,
    pub guild_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<u64>,
    pub action: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, serde_json::Value>,
}

impl Incident {
    pub fn new(kind: impl Into<String>, guild_id: u64, action: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            kind: kind.into(),
            guild_id,
            actor_id: None,
            channel_id: None,
            action: action.into(),
            details: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IncidentSink {
    file: Arc<Mutex<File>>,
}

impl IncidentSink {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create incident directory {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("failed to open incident log {}", path.display()))?;
        Ok(Self { file: Arc::new(Mutex::new(file)) })
    }

    pub async fn record(&self, incident: &Incident) -> Result<()> {
        let mut line = serde_json::to_vec(incident).context("failed to serialize incident")?;
        line.push(b'\n');
        let mut file = self.file.lock().await;
        file.write_all(&line).await.context("failed to write incident")?;
        file.flush().await.context("failed to flush incident")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn appends_valid_json_lines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("incidents.jsonl");
        let sink = IncidentSink::open(&path).await.unwrap();
        sink.record(&Incident::new("test", 1, "logged")).await.unwrap();
        let data = fs::read_to_string(path).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(data.trim()).unwrap();
        assert_eq!(parsed["kind"], "test");
    }
}
