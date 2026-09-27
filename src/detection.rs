use std::collections::HashSet;

use crate::config::RoleGuardConfig;

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
