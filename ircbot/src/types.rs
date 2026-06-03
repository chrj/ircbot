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
    fn nick_and_channel_are_distinct_types() {
        // This is a compile-time guarantee; the assertion just exercises both.
        let n = Nick::from("x");
        let c = Channel::from("#x");
        assert_eq!(n.as_str(), "x");
        assert_eq!(c.as_str(), "#x");
    }
}
