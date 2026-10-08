//! Roles: who may run a command with `role = "..."`.
//!
//! A bot defines a role with its `with_role` method, and the developer chooses
//! how the role matches a sender:
//!
//! ```rust,ignore
//! MyBot::new("mybot", server, ["rust"])
//!     // The hostmask of the sender. Works on each IRC server.
//!     .with_role("op", ["*!*@trusted.host"])
//!     // The services account of the sender. Needs IRCv3 `account-tag`.
//!     .with_role("admin", Role::account(["alice", "bob"]))
//!     // Either of the two.
//!     .with_role("mod", Role::any([
//!         Role::account(["carol"]),
//!         Role::hostmask(["*!*@mod.host"]),
//!     ]))
//! ```
//!
//! # Hostmask or account
//!
//! A hostmask role matches the `nick!user@host` of the sender with glob
//! patterns. It works on each server, but a hostmask is only as safe as the
//! host part: on a network without cloaks, or with a pattern such as
//! `alice!*@*`, someone else can match it.
//!
//! An account role matches the services account that the sender is logged in
//! to. The server gives the account in the `account` tag of each message, with
//! the IRCv3 capability `account-tag`. The bot asks for this capability when a
//! role needs it. Not every server offers it: then `main_loop` refuses to
//! start, with [`StartError::MissingCapability`](crate::StartError::MissingCapability),
//! because the role could never match.

use irc_proto::Message;

use crate::bot::glob_match;
use crate::User;

/// The IRCv3 capability that gives the services account of a sender.
pub(crate) const ACCOUNT_TAG: &str = "account-tag";

/// How a role matches the sender of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Role {
    /// The `nick!user@host` of the sender matches one of these glob patterns
    /// (`*` matches any run of characters, `?` one character). The case does
    /// not matter.
    Hostmask(Vec<String>),
    /// The sender is logged in to one of these services accounts. The case
    /// does not matter. This needs the IRCv3 capability `account-tag`.
    Account(Vec<String>),
    /// One of these roles matches.
    Any(Vec<Role>),
}

impl Role {
    /// A role for the senders whose hostmask matches one of `masks`.
    #[must_use]
    pub fn hostmask(masks: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Role::Hostmask(masks.into_iter().map(Into::into).collect())
    }

    /// A role for the senders who are logged in to one of `accounts`.
    #[must_use]
    pub fn account(accounts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Role::Account(accounts.into_iter().map(Into::into).collect())
    }

    /// A role that matches when one of `roles` matches.
    #[must_use]
    pub fn any(roles: impl IntoIterator<Item = Role>) -> Self {
        Role::Any(roles.into_iter().collect())
    }

    /// Whether `sender`, logged in to `account`, matches the role.
    #[must_use]
    pub(crate) fn matches(&self, sender: Option<&User>, account: Option<&str>) -> bool {
        match self {
            Role::Hostmask(patterns) => sender.is_some_and(|user| {
                let mask = user.hostmask();
                patterns
                    .iter()
                    .any(|pattern| glob_match(pattern, &mask).is_some())
            }),
            Role::Account(accounts) => account.is_some_and(|account| {
                accounts
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(account))
            }),
            Role::Any(roles) => roles.iter().any(|role| role.matches(sender, account)),
        }
    }

    /// Whether the role can only match with the IRCv3 capability
    /// `account-tag`.
    #[must_use]
    pub(crate) fn needs_account(&self) -> bool {
        match self {
            Role::Hostmask(_) => false,
            Role::Account(_) => true,
            Role::Any(roles) => roles.iter().any(Role::needs_account),
        }
    }
}

/// A list of hostmask patterns is a hostmask role, so that
/// `with_role("op", ["*!*@trusted.host"])` works.
impl<T: Into<String>, const N: usize> From<[T; N]> for Role {
    fn from(masks: [T; N]) -> Self {
        Role::hostmask(masks)
    }
}

/// A list of hostmask patterns is a hostmask role.
impl<T: Into<String>> From<Vec<T>> for Role {
    fn from(masks: Vec<T>) -> Self {
        Role::hostmask(masks)
    }
}

/// The first role in `roles` that needs `account-tag`, when `capabilities`
/// does not have it. `None` when no role needs it, or the server gave it.
#[must_use]
pub(crate) fn role_without_account_tag<'a>(
    roles: &'a [(String, Role)],
    capabilities: &[String],
) -> Option<&'a str> {
    if capabilities.iter().any(|cap| cap == ACCOUNT_TAG) {
        return None;
    }
    roles
        .iter()
        .find(|(_, role)| role.needs_account())
        .map(|(name, _)| name.as_str())
}

/// The services account of the sender of `msg`, from its IRCv3 `account` tag.
///
/// The server sends the tag only with the capability `account-tag`, and only
/// for a sender who is logged in.
#[must_use]
pub(crate) fn account_tag(msg: &Message) -> Option<&str> {
    msg.tags
        .as_ref()?
        .iter()
        .find(|tag| tag.0 == "account")
        .and_then(|tag| tag.1.as_deref())
        .filter(|account| !account.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(nick: &str, host: &str) -> User {
        User {
            nick: nick.into(),
            user: "u".to_string(),
            host: host.to_string(),
        }
    }

    #[test]
    fn a_hostmask_role_matches_the_hostmask_of_the_sender() {
        let role = Role::hostmask(["*!*@trusted.host"]);

        assert!(role.matches(Some(&user("alice", "trusted.host")), None));
        assert!(!role.matches(Some(&user("alice", "other.host")), None));
        assert!(!role.matches(None, Some("alice")));
    }

    #[test]
    fn an_account_role_matches_the_account_in_any_case() {
        let role = Role::account(["Alice"]);

        assert!(role.matches(Some(&user("someone", "any.host")), Some("alice")));
        assert!(!role.matches(Some(&user("alice", "any.host")), Some("bob")));
    }

    #[test]
    fn an_account_role_does_not_match_a_sender_who_is_not_logged_in() {
        let role = Role::account(["alice"]);

        // The nick is the name of the account, but no `account` tag came.
        assert!(!role.matches(Some(&user("alice", "any.host")), None));
    }

    #[test]
    fn an_any_role_matches_when_one_role_matches() {
        let role = Role::any([Role::account(["carol"]), Role::hostmask(["*!*@mod.host"])]);

        assert!(role.matches(Some(&user("x", "mod.host")), None));
        assert!(role.matches(Some(&user("x", "other.host")), Some("carol")));
        assert!(!role.matches(Some(&user("x", "other.host")), Some("dave")));
    }

    #[test]
    fn a_role_needs_an_account_when_one_part_is_an_account_role() {
        assert!(!Role::hostmask(["*!*@h"]).needs_account());
        assert!(Role::account(["alice"]).needs_account());
        assert!(Role::any([Role::hostmask(["*!*@h"]), Role::account(["alice"])]).needs_account());
        assert!(!Role::any([Role::hostmask(["*!*@h"])]).needs_account());
    }

    #[test]
    fn a_list_of_patterns_is_a_hostmask_role() {
        assert_eq!(Role::from(["*!*@h"]), Role::hostmask(["*!*@h"]));
        assert_eq!(
            Role::from(vec!["*!*@h".to_string()]),
            Role::hostmask(["*!*@h"])
        );
    }

    #[test]
    fn role_without_account_tag_names_the_first_account_role() {
        let roles = vec![
            ("op".to_string(), Role::hostmask(["*!*@h"])),
            ("admin".to_string(), Role::account(["alice"])),
        ];

        assert_eq!(role_without_account_tag(&roles, &[]), Some("admin"));
        assert_eq!(
            role_without_account_tag(&roles, &["account-tag".to_string()]),
            None
        );
        assert_eq!(role_without_account_tag(&roles[..1], &[]), None);
    }

    #[test]
    fn account_tag_reads_the_account_of_the_sender() {
        let tagged: Message = "@account=alice :alice!a@h PRIVMSG #c :hi".parse().unwrap();
        let other_tag: Message = "@time=2026-10-07T10:00:00Z :alice!a@h PRIVMSG #c :hi"
            .parse()
            .unwrap();
        let untagged: Message = ":alice!a@h PRIVMSG #c :hi".parse().unwrap();

        assert_eq!(account_tag(&tagged), Some("alice"));
        assert_eq!(account_tag(&other_tag), None);
        assert_eq!(account_tag(&untagged), None);
    }
}
