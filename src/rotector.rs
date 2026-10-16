//! Client for the Rayward "Rotector" lookup API (`Rayward API · Rotector` 2.0.0).
//!
//! Only the read-only lookup endpoints Hugh's staff commands need are
//! implemented. Hugh never sends message content to the API; it sends nothing
//! but the account or group identifier a moderator asked about.

use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result, anyhow, bail};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serenity::all::Colour;

use crate::config::RotectorConfig;

pub const WEBSITE_URL: &str = "https://rotector.com";

/// Safety classification shared by the user, group, and Discord lookups. The
/// numeric codes are the API's `flagType` values; their meaning differs a
/// little between resources, so the labels stay deliberately general.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagType {
    Safe,
    Flagged,
    Confirmed,
    Processing,
    Provisional,
    Mixed,
    PastOffender,
    Redacted,
    AwaitingReview,
    Unknown(u8),
}

impl FlagType {
    #[must_use]
    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Safe,
            1 => Self::Flagged,
            2 => Self::Confirmed,
            3 => Self::Processing,
            4 => Self::Provisional,
            5 => Self::Mixed,
            6 => Self::PastOffender,
            8 => Self::Redacted,
            10 => Self::AwaitingReview,
            other => Self::Unknown(other),
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Safe => "No findings",
            Self::Flagged => "Flagged",
            Self::Confirmed => "Confirmed",
            Self::Processing => "Being processed",
            Self::Provisional => "Provisional",
            Self::Mixed => "Mixed findings",
            Self::PastOffender => "Past offender",
            Self::Redacted => "Redacted",
            Self::AwaitingReview => "Awaiting human review",
            Self::Unknown(_) => "Unrecognised status",
        }
    }

    /// False for statuses that are not a finding against the account.
    #[must_use]
    pub const fn is_finding(self) -> bool {
        matches!(
            self,
            Self::Flagged
                | Self::Confirmed
                | Self::Provisional
                | Self::Mixed
                | Self::PastOffender
                | Self::Redacted
        )
    }

    #[must_use]
    pub const fn colour(self) -> Colour {
        match self {
            Self::Safe => Colour::from_rgb(35, 165, 90),
            Self::Confirmed | Self::Redacted => Colour::from_rgb(218, 55, 60),
            Self::Flagged | Self::Mixed => Colour::from_rgb(237, 110, 60),
            Self::Provisional | Self::PastOffender => Colour::from_rgb(240, 178, 50),
            Self::Processing | Self::AwaitingReview | Self::Unknown(_) => {
                Colour::from_rgb(130, 140, 155)
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub label: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub credit: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Reason {
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reviewer {
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedRobloxAccount {
    pub roblox_user_id: u64,
    pub roblox_username: String,
    #[serde(default)]
    pub flag_type: u8,
    #[serde(default)]
    pub detected_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscordUser {
    pub id: String,
    pub flag_type: u8,
    #[serde(default)]
    pub reasons: Vec<Reason>,
    #[serde(default)]
    pub header_message: Option<String>,
    #[serde(default)]
    pub status_label: Option<String>,
    #[serde(default)]
    pub linked_roblox_accounts: Vec<LinkedRobloxAccount>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxUser {
    pub id: u64,
    pub flag_type: u8,
    #[serde(default)]
    pub reasons: Vec<Reason>,
    #[serde(default)]
    pub provisional_reasons: Vec<Reason>,
    #[serde(default)]
    pub reviewer: Option<Reviewer>,
    #[serde(default)]
    pub is_locked: Option<bool>,
    #[serde(default)]
    pub last_updated: Option<i64>,
    #[serde(default)]
    pub record_urls: Vec<String>,
    #[serde(default)]
    pub header_message: Option<String>,
    #[serde(default)]
    pub status_label: Option<String>,
    #[serde(default)]
    pub category_label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxGroup {
    pub id: u64,
    pub flag_type: u8,
    #[serde(default)]
    pub reasons: Vec<Reason>,
    #[serde(default)]
    pub header_message: Option<String>,
    #[serde(default)]
    pub status_label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedServer {
    pub safe_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscordAccountLink {
    pub id: String,
    #[serde(default)]
    pub detected_at: Option<i64>,
    #[serde(default)]
    pub servers: Vec<LinkedServer>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AltAccount {
    pub roblox_user_id: u64,
    pub roblox_username: String,
    #[serde(default)]
    pub detected_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RobloxDiscordLinks {
    pub roblox_user_id: u64,
    #[serde(default)]
    pub discord_accounts: Vec<DiscordAccountLink>,
    #[serde(default)]
    pub alt_accounts: Vec<AltAccount>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub website_url: Option<String>,
    #[serde(default)]
    pub appeal_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    #[serde(default = "no_data")]
    data: Option<T>,
}

/// Field default that does not force a `Default` bound onto `T`.
const fn no_data<T>() -> Option<T> {
    None
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

pub struct RotectorClient {
    http: Client,
    api_key: Arc<str>,
    base_url: String,
    language: String,
}

impl RotectorClient {
    pub fn new(api_key: String, config: &RotectorConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .build()
            .context("failed to create the Rotector HTTP client")?;
        Ok(Self {
            http,
            api_key: Arc::from(api_key),
            base_url: config.base_url.trim().trim_end_matches('/').to_owned(),
            language: config.language.trim().to_owned(),
        })
    }

    pub async fn discord_user(&self, id: u64) -> Result<Option<DiscordUser>> {
        self.get(&format!("/v2/lookup/rotector/discord/user/{id}"))
            .await
    }

    pub async fn roblox_user(&self, id: u64) -> Result<Option<RobloxUser>> {
        self.get(&format!("/v2/lookup/rotector/roblox/user/{id}"))
            .await
    }

    pub async fn roblox_user_links(&self, id: u64) -> Result<Option<RobloxDiscordLinks>> {
        self.get(&format!("/v2/lookup/rotector/roblox/user/{id}/discord"))
            .await
    }

    pub async fn roblox_group(&self, id: u64) -> Result<Option<RobloxGroup>> {
        self.get(&format!("/v2/lookup/rotector/roblox/group/{id}"))
            .await
    }

    pub async fn providers(&self) -> Result<Vec<Provider>> {
        Ok(self
            .get::<Vec<Provider>>("/v2/providers")
            .await?
            .unwrap_or_default())
    }

    /// `Ok(None)` means the API holds no record for that identifier, which is
    /// not the same as a clean result: an unknown account was never analysed.
    async fn get<T>(&self, path: &str) -> Result<Option<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self
            .http
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(&self.api_key)
            .query(&[("lang", self.language.as_str())])
            .send()
            .await
            .context("the Rotector request could not be sent")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("the Rotector response could not be read")?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            bail!(describe_failure(status, &body));
        }
        let envelope: Envelope<T> = serde_json::from_str(&body)
            .context("Rotector returned a response Hugh could not understand")?;
        envelope
            .data
            .map(Some)
            .ok_or_else(|| anyhow!("Rotector returned a success response without any data"))
    }
}

fn describe_failure(status: StatusCode, body: &str) -> String {
    let parsed = serde_json::from_str::<ApiError>(body).ok();
    let detail = parsed.as_ref().and_then(|error| {
        error
            .error
            .as_deref()
            .or(error.code.as_deref())
            .or(error.kind.as_deref())
            .map(str::trim)
            .filter(|detail| !detail.is_empty())
            .map(str::to_owned)
    });
    let summary = match status {
        StatusCode::BAD_REQUEST => "Rotector rejected that identifier",
        StatusCode::UNAUTHORIZED => "Rotector rejected Hugh's API key",
        StatusCode::FORBIDDEN => "Hugh's Rotector key is suspended or not approved for this lookup",
        StatusCode::PAYLOAD_TOO_LARGE => "the Rotector request was too large",
        StatusCode::TOO_MANY_REQUESTS => "Hugh has reached Rotector's rate limit or daily quota",
        StatusCode::SERVICE_UNAVAILABLE => "Rotector is unavailable right now",
        _ if status.is_server_error() => "Rotector reported a server error",
        _ => "the Rotector lookup failed",
    };
    detail.map_or_else(
        || format!("{summary} (HTTP {})", status.as_u16()),
        |detail| format!("{summary}: {detail}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_codes_map_to_findings() {
        assert_eq!(FlagType::from_code(0), FlagType::Safe);
        assert!(!FlagType::from_code(0).is_finding());
        assert!(FlagType::from_code(2).is_finding());
        assert!(!FlagType::from_code(10).is_finding());
        assert_eq!(FlagType::from_code(7), FlagType::Unknown(7));
        assert_eq!(FlagType::from_code(6).label(), "Past offender");
    }

    #[test]
    fn discord_lookup_parses_the_documented_shape() {
        let envelope: Envelope<DiscordUser> = serde_json::from_str(
            r#"{"success":true,"data":{"id":"42","flagType":2,"statusLabel":"Unsafe",
                "reasons":[{"type":"profile","title":"Profile","sources":[
                    {"id":"rotector","label":"Rotector","summary":"Inappropriate profile"}]}],
                "linkedRobloxAccounts":[{"robloxUserId":7,"robloxUsername":"example",
                    "flagType":2,"detectedAt":1700000000,"updatedAt":1700000001}]}}"#,
        )
        .unwrap();
        let data = envelope.data.unwrap();
        assert_eq!(data.flag_type, 2);
        assert_eq!(data.status_label.as_deref(), Some("Unsafe"));
        assert_eq!(data.reasons[0].kind, "profile");
        assert_eq!(data.reasons[0].sources[0].label, "Rotector");
        assert_eq!(data.linked_roblox_accounts[0].roblox_username, "example");
    }

    #[test]
    fn roblox_lookup_keeps_optional_fields_optional() {
        let envelope: Envelope<RobloxUser> =
            serde_json::from_str(r#"{"success":true,"data":{"id":9,"flagType":0}}"#).unwrap();
        let data = envelope.data.unwrap();
        assert_eq!(data.id, 9);
        assert!(data.reasons.is_empty());
        assert!(data.record_urls.is_empty());
        assert!(data.reviewer.is_none());
    }

    #[test]
    fn errors_name_the_cause() {
        let message = describe_failure(
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"type":"RateLimitError","error":"Too many requests"}"#,
        );
        assert!(message.contains("rate limit"));
        assert!(message.contains("Too many requests"));
        let fallback = describe_failure(StatusCode::IM_A_TEAPOT, "not json");
        assert!(fallback.contains("418"));
    }
}
