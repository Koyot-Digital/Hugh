use std::collections::HashSet;

use crate::config::{RoleGuardConfig, ViolationWeights};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    MassMention,
    EveryoneMention,
    ProtectedRolePing,
    MessageFlood,
    DuplicateSpam,
    CoordinatedSpam,
    UnapprovedInvite,
    LinkFlood,
    AttachmentFlood,
    ExcessiveNewlines,
    BlockedDomain,
    ExcessiveCaps,
    RepeatedCharacters,
    EmojiFlood,
    ZalgoText,
    BlockedPhrase,
    ChannelHopSpam,
    NewAccountLink,
}

impl Violation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MassMention => "mass_mention",
            Self::EveryoneMention => "everyone_mention",
            Self::ProtectedRolePing => "protected_role_ping",
            Self::MessageFlood => "message_flood",
            Self::DuplicateSpam => "duplicate_spam",
            Self::CoordinatedSpam => "coordinated_spam",
            Self::UnapprovedInvite => "unapproved_invite",
            Self::LinkFlood => "link_flood",
            Self::AttachmentFlood => "attachment_flood",
            Self::ExcessiveNewlines => "excessive_newlines",
            Self::BlockedDomain => "blocked_domain",
            Self::ExcessiveCaps => "excessive_caps",
            Self::RepeatedCharacters => "repeated_characters",
            Self::EmojiFlood => "emoji_flood",
            Self::ZalgoText => "zalgo_text",
            Self::BlockedPhrase => "blocked_phrase",
            Self::ChannelHopSpam => "channel_hop_spam",
            Self::NewAccountLink => "new_account_link",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::MassMention => "Mass mentions",
            Self::EveryoneMention => "Unauthorised @everyone/@here mention",
            Self::ProtectedRolePing => "Repeated protected-role pings",
            Self::MessageFlood => "Message flood",
            Self::DuplicateSpam => "Repeated-message spam",
            Self::CoordinatedSpam => "Coordinated copy/paste spam",
            Self::UnapprovedInvite => "Unapproved Discord invite",
            Self::LinkFlood => "Too many links",
            Self::AttachmentFlood => "Too many attachments",
            Self::ExcessiveNewlines => "Excessive line breaks",
            Self::BlockedDomain => "Blocked link domain",
            Self::ExcessiveCaps => "Shouting in capitals",
            Self::RepeatedCharacters => "Repeated-character padding",
            Self::EmojiFlood => "Too many emoji",
            Self::ZalgoText => "Stacked combining marks",
            Self::BlockedPhrase => "Blocked phrase",
            Self::ChannelHopSpam => "The same message across several channels",
            Self::NewAccountLink => "A link from a brand-new account",
        }
    }

    /// The configured weight this violation carries, used for both escalation
    /// and the standing cost of the case it produces.
    #[must_use]
    pub const fn weight(self, weights: &ViolationWeights) -> f64 {
        match self {
            Self::MassMention => weights.mass_mention,
            Self::EveryoneMention => weights.everyone_mention,
            Self::ProtectedRolePing => weights.protected_role_ping,
            Self::MessageFlood => weights.message_flood,
            Self::DuplicateSpam => weights.duplicate_spam,
            Self::CoordinatedSpam => weights.coordinated_spam,
            Self::UnapprovedInvite => weights.unapproved_invite,
            Self::LinkFlood => weights.link_flood,
            Self::AttachmentFlood => weights.attachment_flood,
            Self::ExcessiveNewlines => weights.excessive_newlines,
            Self::BlockedDomain => weights.blocked_domain,
            Self::ExcessiveCaps => weights.excessive_caps,
            Self::RepeatedCharacters => weights.repeated_characters,
            Self::EmojiFlood => weights.emoji_flood,
            Self::ZalgoText => weights.zalgo_text,
            Self::BlockedPhrase => weights.blocked_phrase,
            Self::ChannelHopSpam => weights.channel_hop_spam,
            Self::NewAccountLink => weights.new_account_link,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MentionCounts {
    pub direct: usize,
    pub everyone: usize,
}

impl MentionCounts {
    #[must_use]
    pub const fn total(self) -> usize {
        self.direct + self.everyone
    }
}

/// Counts actual Discord user/role mention tokens, including repeated mentions
/// of the same target. Discord's resolved mention arrays contain unique users
/// and roles, so using only those arrays under-counts abusive repeated pings.
#[must_use]
pub fn mention_counts(content: &str, mention_everyone: bool) -> MentionCounts {
    let bytes = content.as_bytes();
    let mut direct = 0;
    let mut index = 0;
    while index + 3 < bytes.len() {
        if bytes[index] != b'<' || bytes[index + 1] != b'@' {
            index += 1;
            continue;
        }
        let mut cursor = index + 2;
        if bytes
            .get(cursor)
            .is_some_and(|byte| matches!(byte, b'!' | b'&'))
        {
            cursor += 1;
        }
        let digits_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor > digits_start && bytes.get(cursor) == Some(&b'>') {
            direct += 1;
            index = cursor + 1;
        } else {
            index += 2;
        }
    }

    let everyone = if mention_everyone {
        content.matches("@everyone").count() + content.matches("@here").count()
    } else {
        0
    };
    MentionCounts {
        direct,
        everyone: everyone.max(usize::from(mention_everyone)),
    }
}

/// Extracts normalized hosts from HTTP(S) and `www.` links without retaining
/// the rest of the URL, which may contain sensitive query parameters.
#[must_use]
pub fn url_hosts(content: &str) -> Vec<String> {
    let lower = content.to_ascii_lowercase();
    let mut hosts = Vec::new();
    let mut cursor = 0;
    while cursor < lower.len() {
        let Some((start, schemeless)) = next_url_start(&lower, cursor) else {
            break;
        };
        let end = lower[start..]
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | '|'))
            .map_or(lower.len(), |length| start + length);
        let raw = lower[start..end]
            .trim_end_matches(['.', ',', '!', '?', ';', ':', '\'', '"', ')', ']', '}']);
        let candidate = if schemeless {
            format!("https://{raw}")
        } else {
            raw.to_owned()
        };
        if let Ok(url) = url::Url::parse(&candidate)
            && let Some(host) = url.host_str()
        {
            hosts.push(host.trim_end_matches('.').to_owned());
        }
        cursor = end.max(start + 1);
    }
    hosts
}

#[must_use]
pub fn first_blocked_domain(hosts: &[String], blocked: &HashSet<String>) -> Option<String> {
    hosts.iter().find_map(|host| {
        blocked.iter().find_map(|configured| {
            let configured = configured.trim().trim_matches('.').to_ascii_lowercase();
            (host.eq_ignore_ascii_case(&configured)
                || host
                    .strip_suffix(&configured)
                    .is_some_and(|prefix| prefix.ends_with('.')))
            .then(|| host.clone())
        })
    })
}

fn next_url_start(content: &str, cursor: usize) -> Option<(usize, bool)> {
    [("https://", false), ("http://", false), ("www.", true)]
        .into_iter()
        .filter_map(|(prefix, schemeless)| find_url_prefix(content, cursor, prefix, schemeless))
        .min_by_key(|(index, _)| *index)
}

fn find_url_prefix(
    content: &str,
    mut cursor: usize,
    prefix: &str,
    schemeless: bool,
) -> Option<(usize, bool)> {
    while let Some(offset) = content[cursor..].find(prefix) {
        let index = cursor + offset;
        let valid_boundary = index == 0
            || !matches!(
                content.as_bytes()[index - 1],
                b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_'
            );
        if !schemeless || valid_boundary {
            return Some((index, schemeless));
        }
        cursor = index + prefix.len();
    }
    None
}

/// Returns the first non-allowlisted Discord invite code in `content`.
#[must_use]
pub fn unapproved_invite(content: &str, allowed_codes: &HashSet<String>) -> Option<String> {
    const PREFIXES: [&str; 3] = [
        "discord.gg/",
        "discord.com/invite/",
        "discordapp.com/invite/",
    ];
    let lower = content.to_ascii_lowercase();

    for prefix in PREFIXES {
        let mut remaining = lower.as_str();
        while let Some(index) = remaining.find(prefix) {
            let after = &remaining[index + prefix.len()..];
            let code: String = after
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
                .collect();
            if !code.is_empty()
                && !allowed_codes
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(&code))
            {
                return Some(code);
            }
            remaining = after.get(code.len()..).unwrap_or_default();
        }
    }
    None
}

/// Characters that carry no visible glyph of their own and are used to pad a
/// message out so a plain substring match misses it.
const INVISIBLE: [char; 7] = [
    '\u{200b}', '\u{200c}', '\u{200d}', '\u{2060}', '\u{feff}', '\u{00ad}', '\u{034f}',
];

/// Strips the invisible padding out, so the phrase list sees what a reader sees.
#[must_use]
pub fn visible_text(content: &str) -> String {
    content
        .chars()
        .filter(|character| !INVISIBLE.contains(character))
        .collect()
}

/// The share of letters that are uppercase. `None` when there are too few
/// letters to judge, which keeps `OK` and `A!` out of it.
#[must_use]
pub fn caps_ratio(content: &str, minimum_letters: usize) -> Option<f64> {
    let letters = content
        .chars()
        .filter(|character| character.is_alphabetic());
    let (total, upper) = letters.fold((0_usize, 0_usize), |(total, upper), character| {
        (total + 1, upper + usize::from(character.is_uppercase()))
    });
    if total < minimum_letters.max(2) {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    Some(upper as f64 / total as f64)
}

/// The longest run of one repeated character, ignoring whitespace runs, which
/// are what ordinary formatting is made of.
#[must_use]
pub fn longest_character_run(content: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    let mut previous = None;
    for character in content.chars() {
        if character.is_whitespace() {
            current = 0;
            previous = None;
            continue;
        }
        if Some(character) == previous {
            current += 1;
        } else {
            current = 1;
            previous = Some(character);
        }
        longest = longest.max(current);
    }
    longest
}

/// Counts emoji: Unicode pictographs plus Discord's own `<:name:id>` and
/// `<a:name:id>` forms.
#[must_use]
pub fn emoji_count(content: &str) -> usize {
    let unicode = content
        .chars()
        .filter(|character| is_emoji(*character))
        .count();
    unicode + custom_emoji_count(content)
}

/// Whether a character is one of the pictographs a message can be padded with.
/// Skin-tone modifiers and variation selectors are deliberately left out so one
/// emoji counts once.
fn is_emoji(character: char) -> bool {
    matches!(
        u32::from(character),
        0x1F300..=0x1FAFF   // pictographs, symbols, extended symbols
            | 0x1F000..=0x1F0FF // mahjong, dominoes, cards
            | 0x1F100..=0x1F1FF // enclosed characters and regional indicators
            | 0x2600..=0x27BF   // misc symbols and dingbats
            | 0x2B00..=0x2BFF   // arrows and geometric shapes
            | 0x1F200..=0x1F2FF // enclosed ideographic supplement
    )
}

/// Counts `<:name:id>` and `<a:name:id>` tokens without allocating.
fn custom_emoji_count(content: &str) -> usize {
    let bytes = content.as_bytes();
    let mut count = 0;
    let mut index = 0;
    while index + 4 < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        let mut cursor = index + 1;
        if bytes.get(cursor) == Some(&b'a') {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b':') {
            index += 1;
            continue;
        }
        cursor += 1;
        let name_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            cursor += 1;
        }
        if cursor == name_start || bytes.get(cursor) != Some(&b':') {
            index += 1;
            continue;
        }
        cursor += 1;
        let id_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor > id_start && bytes.get(cursor) == Some(&b'>') {
            count += 1;
            index = cursor + 1;
        } else {
            index += 1;
        }
    }
    count
}

/// The longest run of combining marks, which is what stacks "zalgo" text into
/// neighbouring lines.
#[must_use]
pub fn longest_combining_run(content: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in content.chars() {
        if is_combining(character) {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn is_combining(character: char) -> bool {
    matches!(
        u32::from(character),
        0x0300..=0x036F   // combining diacritical marks
            | 0x0483..=0x0489 // Cyrillic combining marks
            | 0x0591..=0x05BD // Hebrew points
            | 0x0610..=0x061A // Arabic marks
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06DC
            | 0x0E31 | 0x0E34..=0x0E3A // Thai vowels and tone marks
            | 0x1AB0..=0x1AFF // extended combining marks
            | 0x1DC0..=0x1DFF // supplement
            | 0x20D0..=0x20F0 // combining marks for symbols
            | 0xFE20..=0xFE2F // half marks
    )
}

/// The first blocked phrase a message contains, matched case-insensitively on
/// the message with its invisible padding removed.
#[must_use]
pub fn first_blocked_phrase(content: &str, phrases: &[String]) -> Option<String> {
    if phrases.is_empty() {
        return None;
    }
    let haystack = visible_text(content).to_lowercase();
    phrases.iter().find_map(|phrase| {
        let needle = phrase.trim().to_lowercase();
        (!needle.is_empty() && haystack.contains(&needle)).then(|| phrase.trim().to_owned())
    })
}

#[must_use]
pub fn unauthorized_protected_roles(
    user_id: u64,
    roles: impl IntoIterator<Item = u64>,
    config: &RoleGuardConfig,
) -> Vec<u64> {
    if !config.enabled || config.authorized_member_ids.contains(&user_id) {
        return Vec::new();
    }
    roles
        .into_iter()
        .filter(|role| config.protected_role_ids.contains(role))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_detection_handles_all_supported_forms() {
        let allowed = HashSet::from(["our-server".to_owned()]);
        assert_eq!(
            unapproved_invite("join https://discord.gg/Evil_Code!", &allowed).as_deref(),
            Some("evil_code")
        );
        assert_eq!(
            unapproved_invite("discord.com/invite/our-server", &allowed),
            None
        );
        assert_eq!(unapproved_invite("no invite here", &allowed), None);
    }

    #[test]
    fn role_guard_is_an_explicit_allowlist() {
        let config = RoleGuardConfig {
            enabled: true,
            protected_role_ids: HashSet::from([7, 8]),
            authorized_member_ids: HashSet::from([42]),
        };
        assert_eq!(
            unauthorized_protected_roles(1, [5, 7, 8], &config),
            vec![7, 8]
        );
        assert!(unauthorized_protected_roles(42, [7], &config).is_empty());
    }

    #[test]
    fn repeated_mentions_are_counted_individually() {
        let counts = mention_counts("<@123> <@!123> <@&456> @everyone @everyone", true);
        assert_eq!(counts.direct, 3);
        assert_eq!(counts.everyone, 2);
        assert_eq!(counts.total(), 5);
        assert_eq!(mention_counts("<@nope> @everyone", false).total(), 0);
    }

    #[test]
    fn shouting_is_judged_only_on_long_enough_messages() {
        assert_eq!(caps_ratio("OK", 12), None);
        assert_eq!(caps_ratio("", 12), None);
        // Short of the minimum the message goes unjudged, however it is cased.
        assert_eq!(caps_ratio("FREE NITRO", 12), None);
        // Punctuation and digits are not letters, so they neither help nor hurt.
        let shouted = caps_ratio("FREE NITRO GIVEAWAY!!! 1234", 12).unwrap();
        assert!((shouted - 1.0).abs() < f64::EPSILON, "{shouted}");
        let mixed = caps_ratio("Hello there, this is ordinary", 12).unwrap();
        assert!(mixed < 0.1, "{mixed}");
    }

    #[test]
    fn padding_runs_ignore_whitespace() {
        assert_eq!(longest_character_run("aaaaaaaaaa"), 10);
        assert_eq!(longest_character_run("hi!!!!! there"), 5);
        assert_eq!(longest_character_run("word          word"), 1);
        assert_eq!(longest_character_run(""), 0);
    }

    #[test]
    fn emoji_counting_covers_unicode_and_custom_forms() {
        assert_eq!(emoji_count("hello"), 0);
        assert_eq!(emoji_count("\u{1f600}\u{1f600}\u{2705}"), 3);
        assert_eq!(
            emoji_count("<:flagged:123456789012345678> <a:spin:1234> <:broken:>"),
            2
        );
        // A mention is not an emoji, however much it looks like one.
        assert_eq!(emoji_count("<@123456789012345678>"), 0);
    }

    #[test]
    fn stacked_combining_marks_are_measured_as_runs() {
        assert_eq!(longest_combining_run("plain text"), 0);
        let zalgo = "a\u{0300}\u{0301}\u{0302}\u{0303}\u{0304}\u{0305}\u{0306}b\u{0300}";
        assert_eq!(longest_combining_run(zalgo), 7);
        // One accent per letter is ordinary writing, not stacking.
        assert_eq!(longest_combining_run("e\u{0301}te\u{0301}"), 1);
    }

    #[test]
    fn blocked_phrases_see_through_invisible_padding() {
        let phrases = vec!["free nitro".to_owned(), "  ".to_owned()];
        assert_eq!(
            first_blocked_phrase("Claim your FREE\u{200b} NITRO now", &phrases).as_deref(),
            Some("free nitro")
        );
        assert_eq!(first_blocked_phrase("nothing here", &phrases), None);
        assert_eq!(first_blocked_phrase("free nitro", &[]), None);
    }

    #[test]
    fn links_extract_hosts_and_match_subdomains() {
        let hosts = url_hosts(
            "See [one](https://sub.bad.example/path?q=secret), www.safe.example and HTTP://1.2.3.4/x",
        );
        assert_eq!(hosts, ["sub.bad.example", "www.safe.example", "1.2.3.4"]);
        assert_eq!(
            first_blocked_domain(&hosts, &HashSet::from(["BAD.EXAMPLE".to_owned()])),
            Some("sub.bad.example".to_owned())
        );
        assert_eq!(
            url_hosts("awww.fake www.real.example"),
            ["www.real.example"]
        );
    }
}
