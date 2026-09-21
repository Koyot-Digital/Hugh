use std::collections::HashSet;

use crate::config::RoleGuardConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    MassMention,
    EveryoneMention,
    ProtectedRolePing,
    MessageFlood,
    DuplicateSpam,
    UnapprovedInvite,
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
            Self::UnapprovedInvite => "unapproved_invite",
        }
    }
}

/// Returns the first non-allowlisted Discord invite code in `content`.
#[must_use]
pub fn unapproved_invite(content: &str, allowed_codes: &HashSet<String>) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    const PREFIXES: [&str; 3] = [
        "discord.gg/",
        "discord.com/invite/",
        "discordapp.com/invite/",
    ];

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
}
