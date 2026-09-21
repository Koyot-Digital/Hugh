use std::{
    collections::{HashMap, VecDeque, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use crate::{
    config::Config,
    detection::{Violation, unapproved_invite},
};

#[derive(Debug)]
pub struct MessageInput<'a> {
    pub user_id: u64,
    pub content: &'a str,
    pub total_mentions: usize,
    pub everyone_mentions: usize,
    pub mentioned_roles: &'a [u64],
}

#[derive(Debug, Default)]
pub struct MessageDecision {
    pub violations: Vec<Violation>,
    pub timeout_seconds: u64,
}

impl MessageDecision {
    #[must_use]
    pub const fn should_delete(&self) -> bool {
        !self.violations.is_empty()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct JoinDecision {
    pub raid_active: bool,
    pub raid_activated: bool,
    pub join_count: usize,
    pub account_age_seconds: i64,
    pub young_account: bool,
}

#[derive(Debug, Default)]
struct UserActivity {
    messages: VecDeque<Instant>,
    duplicates: HashMap<u64, VecDeque<Instant>>,
    protected_pings: VecDeque<Instant>,
    last_seen: Option<Instant>,
}

#[derive(Debug, Default)]
struct MessageState {
    users: HashMap<u64, UserActivity>,
    events: u64,
}

#[derive(Debug, Default)]
struct JoinState {
    joins: VecDeque<Instant>,
    raid_until: Option<Instant>,
}

#[derive(Debug)]
pub struct SecurityState {
    message: Mutex<MessageState>,
    joins: Mutex<JoinState>,
    retention: Duration,
}

impl SecurityState {
    #[must_use]
    pub fn new(config: &Config) -> Self {
        let maximum_window = config
            .spam
            .duplicate_window_seconds
            .max(config.spam.window_seconds)
            .max(config.mentions.protected_role_window_seconds)
            .max(60);
        Self {
            message: Mutex::new(MessageState::default()),
            joins: Mutex::new(JoinState::default()),
            retention: Duration::from_secs(maximum_window.saturating_mul(2)),
        }
    }

    pub fn inspect_message(
        &self,
        input: &MessageInput<'_>,
        config: &Config,
        now: Instant,
    ) -> MessageDecision {
        let mut decision = MessageDecision::default();
        let mut state = lock(&self.message);
        state.events = state.events.wrapping_add(1);
        if state.events.is_multiple_of(512) {
            let retention = self.retention;
            state.users.retain(|_, activity| {
                activity.last_seen.is_some_and(|seen| now.saturating_duration_since(seen) <= retention)
            });
        }

        let activity = state.users.entry(input.user_id).or_default();
        activity.last_seen = Some(now);

        if config.mentions.enabled {
            if input.total_mentions > config.mentions.max_total_mentions {
                decision.violations.push(Violation::MassMention);
                decision.timeout_seconds = decision.timeout_seconds.max(config.mentions.timeout_seconds);
            }
            if input.everyone_mentions > config.mentions.max_everyone_mentions {
                decision.violations.push(Violation::EveryoneMention);
                decision.timeout_seconds = decision.timeout_seconds.max(config.mentions.timeout_seconds);
            }
            if input
                .mentioned_roles
                .iter()
                .any(|role| config.mentions.protected_role_ids.contains(role))
            {
                let window = Duration::from_secs(config.mentions.protected_role_window_seconds);
                prune(&mut activity.protected_pings, now, window);
                activity.protected_pings.push_back(now);
                if activity.protected_pings.len() > config.mentions.protected_role_max_pings {
                    decision.violations.push(Violation::ProtectedRolePing);
                    decision.timeout_seconds = decision.timeout_seconds.max(config.mentions.timeout_seconds);
                }
            }
        }

        if config.spam.enabled {
            let flood_window = Duration::from_secs(config.spam.window_seconds);
            prune(&mut activity.messages, now, flood_window);
            activity.messages.push_back(now);
            if activity.messages.len() > config.spam.max_messages {
                decision.violations.push(Violation::MessageFlood);
                decision.timeout_seconds = decision.timeout_seconds.max(config.spam.timeout_seconds);
            }

            let normalized = input.content.trim().to_lowercase();
            if !normalized.is_empty() {
                let hash = hash_message(&normalized);
                let duplicate_window = Duration::from_secs(config.spam.duplicate_window_seconds);
                let occurrences = activity.duplicates.entry(hash).or_default();
                prune(occurrences, now, duplicate_window);
                occurrences.push_back(now);
                if occurrences.len() > config.spam.max_duplicates {
                    decision.violations.push(Violation::DuplicateSpam);
                    decision.timeout_seconds = decision.timeout_seconds.max(config.spam.timeout_seconds);
                }
                activity.duplicates.retain(|_, times| !times.is_empty());
            }
        }

        if config.invites.enabled
            && unapproved_invite(input.content, &config.invites.allowed_codes).is_some()
        {
            decision.violations.push(Violation::UnapprovedInvite);
            decision.timeout_seconds = decision.timeout_seconds.max(config.invites.timeout_seconds);
        }

        decision
    }

    pub fn inspect_join(
        &self,
        config: &Config,
        now: Instant,
        now_unix: i64,
        account_created_unix: i64,
    ) -> JoinDecision {
        let account_age_seconds = now_unix.saturating_sub(account_created_unix).max(0);
        if !config.raids.enabled {
            return JoinDecision {
                raid_active: false,
                raid_activated: false,
                join_count: 0,
                account_age_seconds,
                young_account: false,
            };
        }

        let mut state = lock(&self.joins);
        prune(&mut state.joins, now, Duration::from_secs(config.raids.join_window_seconds));
        state.joins.push_back(now);
        let was_active = state.raid_until.is_some_and(|until| until > now);
        let threshold_reached = state.joins.len() >= config.raids.join_threshold;
        if threshold_reached {
            state.raid_until = Some(now + Duration::from_secs(config.raids.raid_mode_seconds));
        } else if !was_active {
            state.raid_until = None;
        }
        let raid_active = state.raid_until.is_some_and(|until| until > now);

        JoinDecision {
            raid_active,
            raid_activated: !was_active && threshold_reached,
            join_count: state.joins.len(),
            account_age_seconds,
            young_account: account_age_seconds < config.raids.minimum_account_age_seconds,
        }
    }
}

fn prune(queue: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while queue.front().is_some_and(|time| now.saturating_duration_since(*time) > window) {
        queue.pop_front();
    }
}

fn hash_message(message: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    message.hash(&mut hasher);
    hasher.finish()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::config::{
        InviteConfig, MentionConfig, RaidConfig, RoleGuardConfig, SpamConfig, YoungAccountAction,
    };

    fn config() -> Config {
        Config {
            guild_id: 1,
            alert_channel_id: 2,
            incident_log_path: "ignored".into(),
            trusted_user_ids: HashSet::new(),
            trusted_role_ids: HashSet::new(),
            mentions: MentionConfig::default(),
            spam: SpamConfig::default(),
            invites: InviteConfig::default(),
            raids: RaidConfig::default(),
            role_guard: RoleGuardConfig::default(),
        }
    }

    #[test]
    fn third_staff_ping_is_blocked() {
        let mut config = config();
        config.mentions.protected_role_ids.insert(9);
        config.spam.enabled = false;
        let state = SecurityState::new(&config);
        let start = Instant::now();
        let input = MessageInput {
            user_id: 3,
            content: "hello",
            total_mentions: 1,
            everyone_mentions: 0,
            mentioned_roles: &[9],
        };
        assert!(!state.inspect_message(&input, &config, start).should_delete());
        assert!(!state.inspect_message(&input, &config, start + Duration::from_secs(1)).should_delete());
        assert_eq!(
            state.inspect_message(&input, &config, start + Duration::from_secs(2)).violations,
            vec![Violation::ProtectedRolePing]
        );
    }

    #[test]
    fn raid_activates_at_threshold() {
        let mut config = config();
        config.raids = RaidConfig {
            join_threshold: 3,
            ..RaidConfig::default()
        };
        config.raids.young_account_action = YoungAccountAction::Log;
        let state = SecurityState::new(&config);
        let start = Instant::now();
        for offset in 0..2 {
            assert!(!state.inspect_join(&config, start + Duration::from_secs(offset), 100_000, 0).raid_active);
        }
        let result = state.inspect_join(&config, start + Duration::from_secs(2), 100_000, 99_999);
        assert!(result.raid_active);
        assert!(result.raid_activated);
        assert!(result.young_account);
    }
}
