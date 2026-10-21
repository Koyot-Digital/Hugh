use std::{
    collections::{HashMap, VecDeque, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use crate::{
    config::{Config, Strictness},
    detection::{
        Violation, caps_ratio, emoji_count, first_blocked_domain, first_blocked_phrase,
        longest_character_run, longest_combining_run, unapproved_invite, url_hosts,
    },
};

#[derive(Debug)]
pub struct MessageInput<'a> {
    pub user_id: u64,
    pub channel_id: u64,
    pub content: &'a str,
    pub total_mentions: usize,
    pub everyone_mentions: usize,
    pub mentioned_roles: &'a [u64],
    pub attachment_count: usize,
    pub count_towards_rate_limits: bool,
    /// Age of the author's Discord account, for the new-account link guard.
    pub account_age_seconds: i64,
    /// How hard the limits bite for this member, taken from their standing.
    pub strictness: Strictness,
}

/// What Hugh does on top of deleting the message, once a member's running
/// automod weight climbs past the configured steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Escalation {
    Timeout,
    Kick,
    Ban,
}

impl Escalation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Kick => "kick",
            Self::Ban => "ban",
        }
    }
}

#[derive(Debug, Default)]
pub struct MessageDecision {
    pub violations: Vec<Violation>,
    pub timeout_seconds: u64,
    pub link_count: usize,
    pub blocked_domain: Option<String>,
    pub blocked_phrase: Option<String>,
    pub coordinated_users: usize,
    /// Channels this exact message has appeared in inside the hop window.
    pub hop_channels: usize,
    /// Weight of this message's violations.
    pub weight: f64,
    /// The member's rolling automod weight, this message included.
    pub recent_weight: f64,
    pub escalation: Option<Escalation>,
}

impl MessageDecision {
    #[must_use]
    pub const fn should_delete(&self) -> bool {
        !self.violations.is_empty()
    }

    /// Records a violation and the timeout the guard behind it asks for.
    fn flag(&mut self, violation: Violation, timeout_seconds: u64) {
        self.violations.push(violation);
        self.timeout_seconds = self.timeout_seconds.max(timeout_seconds);
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
    /// Channels one message has been seen in, keyed by its normalized hash.
    hops: HashMap<u64, VecDeque<(Instant, u64)>>,
    protected_pings: VecDeque<Instant>,
    /// Weight of recent automod hits, for escalation.
    weights: VecDeque<(Instant, f64)>,
    /// The highest step already taken, so one wave escalates once per level.
    escalated: Option<Escalation>,
    last_seen: Option<Instant>,
}

#[derive(Debug, Default)]
struct MessageState {
    users: HashMap<u64, UserActivity>,
    shared_duplicates: HashMap<u64, VecDeque<(Instant, u64)>>,
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
            .max(config.spam.coordinated_window_seconds)
            .max(config.mentions.protected_role_window_seconds)
            .max(config.automod.channel_hop_window_seconds)
            .max(config.escalation.window_seconds)
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
                activity
                    .last_seen
                    .is_some_and(|seen| now.saturating_duration_since(seen) <= retention)
            });
        }

        let limits = input.strictness;
        let activity = state.users.entry(input.user_id).or_default();
        activity.last_seen = Some(now);

        // One normalization serves the duplicate, coordinated and channel-hop
        // guards, so padding and casing cannot split a wave into three.
        let normalized = normalize_message(input.content);
        let message_hash = (!normalized.is_empty()).then(|| hash_message(&normalized));

        if config.mentions.enabled {
            if input.total_mentions > limits.limit(config.mentions.max_total_mentions) {
                decision.flag(Violation::MassMention, config.mentions.timeout_seconds);
            }
            if input.everyone_mentions > limits.limit(config.mentions.max_everyone_mentions) {
                decision.flag(Violation::EveryoneMention, config.mentions.timeout_seconds);
            }
            if input
                .mentioned_roles
                .iter()
                .any(|role| config.mentions.protected_role_ids.contains(role))
            {
                let window = Duration::from_secs(config.mentions.protected_role_window_seconds);
                prune(&mut activity.protected_pings, now, window);
                activity.protected_pings.push_back(now);
                if activity.protected_pings.len()
                    > limits.limit(config.mentions.protected_role_max_pings)
                {
                    decision.flag(Violation::ProtectedRolePing, config.mentions.timeout_seconds);
                }
            }
        }

        if config.spam.enabled && input.count_towards_rate_limits {
            let flood_window = Duration::from_secs(config.spam.window_seconds);
            prune(&mut activity.messages, now, flood_window);
            activity.messages.push_back(now);
            if activity.messages.len() > limits.limit(config.spam.max_messages) {
                decision.flag(Violation::MessageFlood, config.spam.timeout_seconds);
            }

            if let Some(hash) = message_hash {
                let duplicate_window = Duration::from_secs(config.spam.duplicate_window_seconds);
                activity.duplicates.retain(|_, times| {
                    prune(times, now, duplicate_window);
                    !times.is_empty()
                });
                let occurrences = activity.duplicates.entry(hash).or_default();
                prune(occurrences, now, duplicate_window);
                occurrences.push_back(now);
                if occurrences.len() > limits.limit(config.spam.max_duplicates) {
                    decision.flag(Violation::DuplicateSpam, config.spam.timeout_seconds);
                }

                if normalized.chars().count() >= config.spam.coordinated_min_length {
                    let coordinated_window =
                        Duration::from_secs(config.spam.coordinated_window_seconds);
                    state.shared_duplicates.retain(|_, occurrences| {
                        prune_pairs(occurrences, now, coordinated_window);
                        !occurrences.is_empty()
                    });
                    let occurrences = state.shared_duplicates.entry(hash).or_default();
                    prune_pairs(occurrences, now, coordinated_window);
                    occurrences.retain(|(_, user_id)| *user_id != input.user_id);
                    occurrences.push_back((now, input.user_id));
                    decision.coordinated_users = occurrences.len();
                    if occurrences.len() > limits.limit(config.spam.max_coordinated_duplicates) {
                        decision.flag(Violation::CoordinatedSpam, config.spam.timeout_seconds);
                    }
                }
            }
        }

        // Channel hopping belongs to the automod section rather than the spam
        // one: the member is not flooding a channel, they are walking a single
        // message around the server.
        if config.automod.enabled
            && config.automod.channel_hop_threshold > 0
            && input.count_towards_rate_limits
            && let Some(hash) = message_hash
        {
            let activity = state.users.entry(input.user_id).or_default();
            let window = Duration::from_secs(config.automod.channel_hop_window_seconds);
            activity.hops.retain(|_, seen| {
                prune_pairs(seen, now, window);
                !seen.is_empty()
            });
            let seen = activity.hops.entry(hash).or_default();
            prune_pairs(seen, now, window);
            if !seen.iter().any(|(_, channel)| *channel == input.channel_id) {
                seen.push_back((now, input.channel_id));
            }
            decision.hop_channels = seen.len();
            // Never below two channels: one channel is simply a message.
            let threshold = limits.limit(config.automod.channel_hop_threshold).max(2);
            if seen.len() >= threshold {
                decision.flag(Violation::ChannelHopSpam, config.automod.timeout_seconds);
            }
        }

        drop(state);
        inspect_stateless(input, config, &mut decision);
        self.accumulate(input, config, &mut decision, now);
        decision.timeout_seconds = limits.duration(decision.timeout_seconds);

        decision
    }

    /// Adds this message's weight to the member's rolling total and decides
    /// whether that total has earned a step up from deletion.
    fn accumulate(
        &self,
        input: &MessageInput<'_>,
        config: &Config,
        decision: &mut MessageDecision,
        now: Instant,
    ) {
        if decision.violations.is_empty() {
            return;
        }
        decision.weight = decision
            .violations
            .iter()
            .map(|violation| violation.weight(&config.standing.violations))
            .sum();
        decision.recent_weight = decision.weight;
        if !config.escalation.enabled {
            return;
        }

        let mut state = lock(&self.message);
        let activity = state.users.entry(input.user_id).or_default();
        let window = Duration::from_secs(config.escalation.window_seconds);
        prune_pairs(&mut activity.weights, now, window);
        // Once the window has emptied the member starts over, so a quiet month
        // is not followed by an instant ban for one slip.
        if activity.weights.is_empty() {
            activity.escalated = None;
        }
        activity.weights.push_back((now, decision.weight));
        let total: f64 = activity.weights.iter().map(|(_, weight)| *weight).sum();
        decision.recent_weight = total;

        let limits = input.strictness;
        let reached = |at: f64| at > 0.0 && total >= limits.threshold(at);
        let step = if reached(config.escalation.ban_at) {
            Some(Escalation::Ban)
        } else if reached(config.escalation.kick_at) {
            Some(Escalation::Kick)
        } else if reached(config.escalation.timeout_at) {
            Some(Escalation::Timeout)
        } else {
            None
        };
        if let Some(step) = step
            && activity.escalated < Some(step)
        {
            activity.escalated = Some(step);
            decision.escalation = Some(step);
        }
        drop(state);

        if decision.escalation == Some(Escalation::Timeout) {
            decision.timeout_seconds = decision
                .timeout_seconds
                .max(config.escalation.timeout_seconds);
        }
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
        prune(
            &mut state.joins,
            now,
            Duration::from_secs(config.raids.join_window_seconds),
        );
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

fn inspect_stateless(input: &MessageInput<'_>, config: &Config, decision: &mut MessageDecision) {
    if config.invites.enabled
        && unapproved_invite(input.content, &config.invites.allowed_codes).is_some()
    {
        decision.flag(Violation::UnapprovedInvite, config.invites.timeout_seconds);
    }
    // Hosts are parsed once: the content guard, the new-account guard and the
    // evidence on the case card all want the same answer.
    let hosts = url_hosts(input.content);
    decision.link_count = hosts.len();
    inspect_content(input, config, decision, &hosts);
    inspect_automod(input, config, decision);
}

fn inspect_content(
    input: &MessageInput<'_>,
    config: &Config,
    decision: &mut MessageDecision,
    hosts: &[String],
) {
    if !config.content.enabled {
        return;
    }
    let limits = input.strictness;
    if hosts.len() > limits.limit(config.content.max_links) {
        decision.flag(Violation::LinkFlood, config.content.timeout_seconds);
    }
    if input.attachment_count > limits.limit(config.content.max_attachments) {
        decision.flag(Violation::AttachmentFlood, config.content.timeout_seconds);
    }
    if input.content.matches('\n').count() > limits.limit(config.content.max_newlines) {
        decision.flag(Violation::ExcessiveNewlines, config.content.timeout_seconds);
    }
    decision.blocked_domain = first_blocked_domain(hosts, &config.content.blocked_domains);
    if decision.blocked_domain.is_some() {
        decision.flag(Violation::BlockedDomain, config.content.timeout_seconds);
    }
}

/// The shape-of-the-message guards: shouting, padding, emoji walls, stacked
/// combining marks, blocked phrases, and links from an account minutes old.
fn inspect_automod(input: &MessageInput<'_>, config: &Config, decision: &mut MessageDecision) {
    let automod = &config.automod;
    if !automod.enabled {
        return;
    }
    let limits = input.strictness;
    let timeout = automod.timeout_seconds;

    if let Some(phrase) = first_blocked_phrase(input.content, &automod.blocked_phrases) {
        decision.blocked_phrase = Some(phrase);
        decision.flag(Violation::BlockedPhrase, timeout);
    }
    if automod.max_caps_ratio < 1.0
        && let Some(ratio) = caps_ratio(input.content, automod.caps_min_letters)
        && ratio > limits.ratio(automod.max_caps_ratio)
    {
        decision.flag(Violation::ExcessiveCaps, timeout);
    }
    if automod.max_repeated_characters > 0
        && longest_character_run(input.content) > limits.limit(automod.max_repeated_characters)
    {
        decision.flag(Violation::RepeatedCharacters, timeout);
    }
    if automod.max_emoji > 0 && emoji_count(input.content) > limits.limit(automod.max_emoji) {
        decision.flag(Violation::EmojiFlood, timeout);
    }
    if automod.max_combining_marks > 0
        && longest_combining_run(input.content) > limits.limit(automod.max_combining_marks)
    {
        decision.flag(Violation::ZalgoText, timeout);
    }
    if automod.new_account_link_age_seconds > 0
        && decision.link_count > 0
        && input.account_age_seconds < automod.new_account_link_age_seconds
    {
        decision.flag(Violation::NewAccountLink, timeout);
    }
}

fn prune(queue: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while queue
        .front()
        .is_some_and(|time| now.saturating_duration_since(*time) > window)
    {
        queue.pop_front();
    }
}

/// Drops the timestamped entries that have aged out of `window`.
fn prune_pairs<T>(queue: &mut VecDeque<(Instant, T)>, now: Instant, window: Duration) {
    while queue
        .front()
        .is_some_and(|(time, _)| now.saturating_duration_since(*time) > window)
    {
        queue.pop_front();
    }
}

fn normalize_message(message: &str) -> String {
    message
        .chars()
        .filter(|ch| {
            !matches!(
                ch,
                '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
            )
        })
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn hash_message(message: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    message.hash(&mut hasher);
    hasher.finish()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RaidConfig, YoungAccountAction};

    /// Defaults for every section, so adding a guard cannot break these tests
    /// merely by existing.
    fn config() -> Config {
        let config: Config = toml::from_str(
            "guild_id = \"1\"\nincident_channel_id = \"2\"\n\n[role_guard]\nenabled = false",
        )
        .unwrap();
        config.validate().unwrap();
        config
    }

    /// A message from a member in good standing, in one channel, from an
    /// account old enough that the new-account guards stay out of the way.
    fn message<'a>(user_id: u64, content: &'a str) -> MessageInput<'a> {
        MessageInput {
            user_id,
            channel_id: 100,
            content,
            total_mentions: 0,
            everyone_mentions: 0,
            mentioned_roles: &[],
            attachment_count: 0,
            count_towards_rate_limits: true,
            account_age_seconds: 31_536_000,
            strictness: Strictness::default(),
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
            total_mentions: 1,
            mentioned_roles: &[9],
            ..message(3, "hello")
        };
        assert!(
            !state
                .inspect_message(&input, &config, start)
                .should_delete()
        );
        assert!(
            !state
                .inspect_message(&input, &config, start + Duration::from_secs(1))
                .should_delete()
        );
        assert_eq!(
            state
                .inspect_message(&input, &config, start + Duration::from_secs(2))
                .violations,
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
            assert!(
                !state
                    .inspect_join(&config, start + Duration::from_secs(offset), 100_000, 0)
                    .raid_active
            );
        }
        let result = state.inspect_join(&config, start + Duration::from_secs(2), 100_000, 99_999);
        assert!(result.raid_active);
        assert!(result.raid_activated);
        assert!(result.young_account);
    }

    #[test]
    fn flood_limit_is_inclusive_and_expires() {
        let mut config = config();
        config.mentions.enabled = false;
        config.spam.max_messages = 2;
        config.spam.max_duplicates = 10;
        config.spam.window_seconds = 5;
        let state = SecurityState::new(&config);
        let start = Instant::now();
        let input = message(3, "one");

        assert!(
            !state
                .inspect_message(&input, &config, start)
                .should_delete()
        );
        assert!(
            !state
                .inspect_message(&input, &config, start + Duration::from_secs(1))
                .should_delete()
        );
        assert!(
            state
                .inspect_message(&input, &config, start + Duration::from_secs(2))
                .violations
                .contains(&Violation::MessageFlood)
        );
        assert!(
            !state
                .inspect_message(&input, &config, start + Duration::from_secs(8))
                .should_delete()
        );
    }

    #[test]
    fn invite_guard_respects_allowlist() {
        let mut config = config();
        config.mentions.enabled = false;
        config.spam.enabled = false;
        config.invites.enabled = true;
        config.invites.allowed_codes.insert("home".into());
        let state = SecurityState::new(&config);
        let start = Instant::now();

        let allowed = message(3, "https://discord.gg/home");
        assert!(
            !state
                .inspect_message(&allowed, &config, start)
                .should_delete()
        );

        let blocked = MessageInput {
            content: "https://discord.gg/other",
            ..allowed
        };
        assert_eq!(
            state.inspect_message(&blocked, &config, start).violations,
            vec![Violation::UnapprovedInvite]
        );
    }

    #[test]
    fn coordinated_spam_uses_distinct_users_and_normalized_text() {
        let mut config = config();
        config.mentions.enabled = false;
        config.content.enabled = false;
        config.spam.max_messages = 100;
        config.spam.max_duplicates = 100;
        config.spam.max_coordinated_duplicates = 2;
        config.spam.coordinated_min_length = 5;
        let state = SecurityState::new(&config);
        let start = Instant::now();

        for (offset, user_id, content) in [
            (0, 1, "Raid message"),
            (1, 1, " raid\u{200b}   MESSAGE "),
            (2, 2, "RAID MESSAGE"),
        ] {
            let input = message(user_id, content);
            assert!(
                !state
                    .inspect_message(&input, &config, start + Duration::from_secs(offset))
                    .violations
                    .contains(&Violation::CoordinatedSpam)
            );
        }
        let input = message(3, "raid message");
        let decision = state.inspect_message(&input, &config, start + Duration::from_secs(3));
        assert!(decision.violations.contains(&Violation::CoordinatedSpam));
        assert_eq!(decision.coordinated_users, 3);
    }

    #[test]
    fn content_guard_reports_blocked_domains_and_limits() {
        let mut config = config();
        config.mentions.enabled = false;
        config.spam.enabled = false;
        config.invites.enabled = false;
        config.content.max_links = 1;
        config.content.max_attachments = 1;
        config.content.max_newlines = 1;
        config
            .content
            .blocked_domains
            .insert("bad.example".to_owned());
        let state = SecurityState::new(&config);
        let input = MessageInput {
            attachment_count: 2,
            count_towards_rate_limits: false,
            ..message(3, "https://safe.example\nline\nhttps://sub.bad.example/path")
        };
        let decision = state.inspect_message(&input, &config, Instant::now());
        assert!(decision.violations.contains(&Violation::LinkFlood));
        assert!(decision.violations.contains(&Violation::AttachmentFlood));
        assert!(decision.violations.contains(&Violation::ExcessiveNewlines));
        assert!(decision.violations.contains(&Violation::BlockedDomain));
        assert_eq!(decision.blocked_domain.as_deref(), Some("sub.bad.example"));
    }

    #[test]
    fn edits_do_not_inflate_rate_limit_windows() {
        let mut config = config();
        config.mentions.enabled = false;
        config.invites.enabled = false;
        config.content.enabled = false;
        config.spam.max_messages = 1;
        config.spam.max_duplicates = 10;
        config.spam.max_coordinated_duplicates = 10;
        let state = SecurityState::new(&config);
        let start = Instant::now();
        let mut input = message(3, "ordinary message");

        assert!(
            !state
                .inspect_message(&input, &config, start)
                .should_delete()
        );
        input.count_towards_rate_limits = false;
        assert!(
            !state
                .inspect_message(&input, &config, start + Duration::from_secs(1))
                .should_delete()
        );
        input.count_towards_rate_limits = true;
        assert!(
            state
                .inspect_message(&input, &config, start + Duration::from_secs(2))
                .violations
                .contains(&Violation::MessageFlood)
        );
    }

    /// The same three links pass for a member in good standing and fail for one
    /// whose record has caught up with them, and the timeout stretches to match.
    #[test]
    fn standing_scales_both_the_limits_and_the_timeout() {
        let mut config = config();
        config.spam.enabled = false;
        config.automod.enabled = false;
        config.escalation.enabled = false;
        config.content.max_links = 4;
        config.content.timeout_seconds = 600;
        let state = SecurityState::new(&config);
        let now = Instant::now();
        let mut input = message(3, "https://a.example https://b.example https://c.example");

        assert!(!state.inspect_message(&input, &config, now).should_delete());

        input.strictness = Strictness {
            thresholds: 0.4,
            timeouts: 2.5,
        };
        let decision = state.inspect_message(&input, &config, now);
        assert!(decision.violations.contains(&Violation::LinkFlood));
        assert_eq!(decision.timeout_seconds, 1_500);
    }

    #[test]
    fn automod_catches_shouting_padding_emoji_and_stacked_marks() {
        let mut config = config();
        config.mentions.enabled = false;
        config.spam.enabled = false;
        config.escalation.enabled = false;
        config.automod.max_caps_ratio = 0.7;
        config.automod.max_repeated_characters = 10;
        config.automod.max_emoji = 12;
        config.automod.max_combining_marks = 6;
        let state = SecurityState::new(&config);
        let now = Instant::now();

        let shouting = message(3, "STOP READING THIS RIGHT NOW");
        assert!(
            state
                .inspect_message(&shouting, &config, now)
                .violations
                .contains(&Violation::ExcessiveCaps)
        );

        let padded = message(4, "aaaaaaaaaaaaaaaaaa");
        assert!(
            state
                .inspect_message(&padded, &config, now)
                .violations
                .contains(&Violation::RepeatedCharacters)
        );

        let wall = "\u{1f600}".repeat(13);
        let emoji = message(5, &wall);
        assert!(
            state
                .inspect_message(&emoji, &config, now)
                .violations
                .contains(&Violation::EmojiFlood)
        );

        let stacked = format!("h{}i", "\u{0301}".repeat(7));
        let zalgo = message(6, &stacked);
        assert!(
            state
                .inspect_message(&zalgo, &config, now)
                .violations
                .contains(&Violation::ZalgoText)
        );

        // Ordinary writing trips none of them.
        let ordinary = message(7, "Hello, is anyone around to help with a build?");
        assert!(!state.inspect_message(&ordinary, &config, now).should_delete());
    }

    #[test]
    fn blocked_phrases_and_fresh_account_links_are_caught() {
        let mut config = config();
        config.mentions.enabled = false;
        config.spam.enabled = false;
        config.escalation.enabled = false;
        config.automod.blocked_phrases = vec!["free nitro".to_owned()];
        config.automod.new_account_link_age_seconds = 86_400;
        let state = SecurityState::new(&config);

        let input = MessageInput {
            account_age_seconds: 600,
            ..message(3, "free nitro here: https://scam.example")
        };
        let decision = state.inspect_message(&input, &config, Instant::now());
        assert!(decision.violations.contains(&Violation::BlockedPhrase));
        assert!(decision.violations.contains(&Violation::NewAccountLink));
        assert_eq!(decision.blocked_phrase.as_deref(), Some("free nitro"));

        // The same link from an established account is nobody's business.
        let settled = message(4, "look at https://scam.example");
        assert!(
            !state
                .inspect_message(&settled, &config, Instant::now())
                .should_delete()
        );
    }

    #[test]
    fn the_same_message_in_three_channels_is_hopping() {
        let mut config = config();
        config.mentions.enabled = false;
        config.spam.enabled = false;
        config.escalation.enabled = false;
        config.automod.channel_hop_threshold = 3;
        let state = SecurityState::new(&config);
        let start = Instant::now();
        let mut input = message(3, "come and join my server");

        for (offset, channel) in [(0, 100), (1, 200)] {
            input.channel_id = channel;
            assert!(
                !state
                    .inspect_message(&input, &config, start + Duration::from_secs(offset))
                    .should_delete()
            );
        }
        input.channel_id = 300;
        let decision = state.inspect_message(&input, &config, start + Duration::from_secs(2));
        assert!(decision.violations.contains(&Violation::ChannelHopSpam));
        assert_eq!(decision.hop_channels, 3);
    }

    /// Escalation climbs one step at a time, never repeats a step it has
    /// already taken, and carries the weight that got it there.
    #[test]
    fn escalation_steps_up_once_per_level() {
        let mut config = config();
        config.spam.enabled = false;
        config.content.enabled = false;
        config.automod.enabled = false;
        config.standing.violations.mass_mention = 6.0;
        config.escalation.timeout_at = 5.0;
        config.escalation.kick_at = 10.0;
        config.escalation.ban_at = 20.0;
        config.escalation.timeout_seconds = 3_600;
        let state = SecurityState::new(&config);
        let start = Instant::now();
        let input = MessageInput {
            total_mentions: 10,
            ..message(3, "look at me")
        };

        let first = state.inspect_message(&input, &config, start);
        assert_eq!(first.escalation, Some(Escalation::Timeout));
        assert!((first.weight - 6.0).abs() < f64::EPSILON);
        assert!((first.recent_weight - 6.0).abs() < f64::EPSILON);
        assert_eq!(first.timeout_seconds, 3_600);

        let steps: Vec<Option<Escalation>> = (1..4)
            .map(|offset| {
                state
                    .inspect_message(&input, &config, start + Duration::from_secs(offset))
                    .escalation
            })
            .collect();
        assert_eq!(
            steps,
            vec![Some(Escalation::Kick), None, Some(Escalation::Ban)]
        );
    }

    /// A clean message leaves no weight behind, so nothing accumulates towards
    /// an escalation for a member who is simply talking.
    #[test]
    fn clean_messages_carry_no_weight() {
        let config = config();
        let state = SecurityState::new(&config);
        let decision = state.inspect_message(&message(3, "morning"), &config, Instant::now());
        assert!(!decision.should_delete());
        assert!((decision.weight - 0.0).abs() < f64::EPSILON);
        assert_eq!(decision.escalation, None);
    }
}
