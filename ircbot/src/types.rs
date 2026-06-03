//! Strongly-typed wrappers for IRC identifiers.
//!
//! IRC nicks and channel names are both "just strings" on the wire, which makes
//! them easy to mix up in function signatures and stored state. [`Nick`] and
//! [`Channel`] are thin newtypes that keep the two distinct in the type system
//! while still behaving like the string they wrap (they implement [`Display`],
//! `From<&str>`/`From<String>`, and compare directly against string slices).
//!
//! [`Display`]: std::fmt::Display

use std::fmt;

use irc_proto::chan::ChannelExt;

/// Define a string newtype with the conversions and comparisons the crate
/// relies on. Kept private so `Nick` and `Channel` stay distinct types.
macro_rules! string_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            /// Borrow the wrapped value as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the newtype, returning the wrapped `String`.
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                $name(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                $name(s.to_string())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }
    };
}

string_newtype! {
    /// An IRC nickname.
    Nick
}

string_newtype! {
    /// An IRC channel name, including its prefix (e.g. `#rust`).
    Channel
}

/// The destination of a message: either a [`Channel`] or a [`User`](crate::User)
/// (a private query, identified by [`Nick`]).
///
/// Modelling the destination as an enum makes the channel-vs-query distinction
/// part of the type, so a message can never be flagged as "to a channel" while
/// carrying a nick (or vice versa).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// A channel, e.g. `#rust`.
    Channel(Channel),
    /// A single user, addressed by nick (a private query).
    User(Nick),
}

impl Target {
    /// Build a `Target` from a raw IRC target string, choosing the variant from
    /// the channel-prefix rules (`#`, `&`, `+`, `!`). Anything else — including
    /// the empty string used by target-less cron handlers — is treated as a
    /// [`Target::User`].
    #[must_use]
    pub fn from_raw(target: &str) -> Self {
        if target.is_channel_name() {
            Target::Channel(Channel::from(target))
        } else {
            Target::User(Nick::from(target))
        }
    }

    /// Whether this destination is a channel (rather than a private query).
    #[must_use]
    pub fn is_channel(&self) -> bool {
        matches!(self, Target::Channel(_))
    }

    /// Borrow the underlying name (channel or nick) as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Target::Channel(c) => c.as_str(),
            Target::User(n) => n.as_str(),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_and_display_round_trip() {
        let n = Nick::from("alice");
        assert_eq!(n.as_str(), "alice");
        assert_eq!(n.to_string(), "alice");
    }

    #[test]
    fn from_string_and_str_are_equivalent() {
        assert_eq!(Channel::from("#rust"), Channel::from("#rust".to_string()));
    }

    #[test]
    fn compares_against_str_slices() {
        assert_eq!(Nick::from("bob"), "bob");
        assert_ne!(Nick::from("bob"), "alice");
    }

    #[test]
    fn into_string_unwraps() {
        assert_eq!(Channel::from("#a").into_string(), "#a".to_string());
    }

    #[test]
    fn target_from_raw_picks_variant_by_prefix() {
        assert_eq!(
            Target::from_raw("#rust"),
            Target::Channel(Channel::from("#rust"))
        );
        assert_eq!(Target::from_raw("alice"), Target::User(Nick::from("alice")));
        // Empty (target-less cron) is treated as a user.
        assert!(!Target::from_raw("").is_channel());
    }

    #[test]
    fn target_as_str_and_display_delegate_to_inner() {
        let t = Target::Channel(Channel::from("#a"));
        assert_eq!(t.as_str(), "#a");
        assert_eq!(t.to_string(), "#a");
        assert!(t.is_channel());
    }

    #[test]
    fn nick_and_channel_are_distinct_types() {
        // This is a compile-time guarantee; the assertion just exercises both.
        let n = Nick::from("x");
        let c = Channel::from("#x");
        assert_eq!(n.as_str(), "x");
        assert_eq!(c.as_str(), "#x");
    }
}
