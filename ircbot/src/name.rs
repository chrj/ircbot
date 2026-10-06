//! The rule for the names of plugins and store namespaces.
//!
//! A plugin uses its name as its store namespace, so both follow one rule.

/// Whether `name` obeys the rule for a plugin or namespace name.
///
/// A name starts with a lowercase ASCII letter. The other characters are
/// lowercase ASCII letters, digits, and `_`. Thus a name is safe to use in SQL
/// as a prefix of a table name. SQLite refuses a table name that starts with
/// `sqlite_`, so `sqlite` and names that start with `sqlite_` are refused.
///
/// The `#[plugin]` macro checks the same rule at compile time. Keep
/// `is_valid_plugin_name` in `ircbot-macros/src/lib.rs` the same as this
/// function.
pub(crate) fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let reserved = name == "sqlite" || name.starts_with("sqlite_");
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !reserved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_rule() {
        let cases = [
            ("quotes", true),
            ("seen_v2", true),
            ("a", true),
            ("", false),
            ("Quotes", false),
            ("2quotes", false),
            ("_ircbot", false),
            ("quo-tes", false),
            ("quo tes", false),
            ("quotes;", false),
            ("cité", false),
            ("sqlite", false),
            ("sqlite_stat", false),
            ("sqlitefoo", true),
        ];
        for (name, valid) in cases {
            assert_eq!(is_valid_name(name), valid, "name {name:?}");
        }
    }
}
