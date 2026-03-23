use irc_proto::Message;
use ircbot::bot::{check_trigger, glob_match};
use ircbot::handler::Trigger;

// ─── glob_match ───────────────────────────────────────────────────────────────

#[test]
fn glob_exact_match() {
    assert!(glob_match("hello", "hello").is_some());
    assert!(glob_match("hello", "world").is_none());
}

#[test]
fn glob_single_wildcard_suffix() {
    let caps = glob_match("hello *", "hello world").unwrap();
    assert_eq!(caps, vec!["world"]);
}

#[test]
fn glob_single_wildcard_prefix() {
    let caps = glob_match("* world", "hello world").unwrap();
    assert_eq!(caps, vec!["hello"]);
}

#[test]
fn glob_two_wildcards() {
    let caps = glob_match("* loves *", "alice loves rust").unwrap();
    assert_eq!(caps, vec!["alice", "rust"]);
}

#[test]
fn glob_no_wildcard_mismatch() {
    assert!(glob_match("hello world", "hello there").is_none());
}

#[test]
fn glob_empty_capture() {
    // Pattern ends with '*', empty trailing text is a valid capture.
    let caps = glob_match("hello *", "hello ").unwrap();
    assert_eq!(caps, vec![""]);
}

#[test]
fn glob_case_insensitive() {
    // The generated regex uses (?i)
    assert!(glob_match("Hello *", "hello world").is_some());
}

// ─── check_trigger: Command ───────────────────────────────────────────────────

fn privmsg(target: &str, text: &str) -> Message {
    format!(":nick!u@h PRIVMSG {} :{}", target, text)
        .parse()
        .unwrap()
}

#[test]
fn command_trigger_basic() {
    let trigger = Trigger::Command {
        name: "ping".to_string(),
        target: None,
    };
    let msg = privmsg("#chan", "!ping");
    let caps = check_trigger(&trigger, &msg, "bot").unwrap();
    assert!(caps.is_empty());
}

#[test]
fn command_trigger_with_args() {
    let trigger = Trigger::Command {
        name: "echo".to_string(),
        target: None,
    };
    let msg = privmsg("#chan", "!echo hello world");
    let caps = check_trigger(&trigger, &msg, "bot").unwrap();
    assert_eq!(caps, vec!["hello world"]);
}

#[test]
fn command_trigger_wrong_name() {
    let trigger = Trigger::Command {
        name: "ping".to_string(),
        target: None,
    };
    let msg = privmsg("#chan", "!pong");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn command_trigger_target_match() {
    let trigger = Trigger::Command {
        name: "hi".to_string(),
        target: Some("#general".to_string()),
    };
    assert!(check_trigger(&trigger, &privmsg("#general", "!hi"), "bot").is_some());
    assert!(check_trigger(&trigger, &privmsg("#other", "!hi"), "bot").is_none());
}

#[test]
fn command_trigger_case_insensitive_name() {
    let trigger = Trigger::Command {
        name: "Ping".to_string(),
        target: None,
    };
    assert!(check_trigger(&trigger, &privmsg("#chan", "!ping"), "bot").is_some());
    assert!(check_trigger(&trigger, &privmsg("#chan", "!PING"), "bot").is_some());
}

#[test]
fn command_trigger_ignores_non_privmsg() {
    let trigger = Trigger::Command {
        name: "ping".to_string(),
        target: None,
    };
    let msg = ":nick!u@h JOIN #chan".parse().unwrap();
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

// ─── check_trigger: Message ───────────────────────────────────────────────────

#[test]
fn message_trigger_exact() {
    let trigger = Trigger::Message {
        pattern: "hello".to_string(),
        target: None,
    };
    assert!(check_trigger(&trigger, &privmsg("#chan", "hello"), "bot").is_some());
    assert!(check_trigger(&trigger, &privmsg("#chan", "hello world"), "bot").is_none());
}

#[test]
fn message_trigger_wildcard() {
    let trigger = Trigger::Message {
        pattern: "hello *".to_string(),
        target: None,
    };
    let caps = check_trigger(&trigger, &privmsg("#chan", "hello alice"), "bot").unwrap();
    assert_eq!(caps, vec!["alice"]);
}

#[test]
fn message_trigger_target_filter() {
    let trigger = Trigger::Message {
        pattern: "hi".to_string(),
        target: Some("#rust".to_string()),
    };
    assert!(check_trigger(&trigger, &privmsg("#rust", "hi"), "bot").is_some());
    assert!(check_trigger(&trigger, &privmsg("#other", "hi"), "bot").is_none());
}

// ─── check_trigger: Event ────────────────────────────────────────────────────

#[test]
fn event_trigger_join() {
    let trigger = Trigger::Event {
        event: "JOIN".to_string(),
        target: None,
        regex: None,
    };
    let msg = ":nick!u@h JOIN #chan".parse().unwrap();
    assert!(check_trigger(&trigger, &msg, "bot").is_some());
}

#[test]
fn event_trigger_join_wrong_event() {
    let trigger = Trigger::Event {
        event: "PART".to_string(),
        target: None,
        regex: None,
    };
    let msg = ":nick!u@h JOIN #chan".parse().unwrap();
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn event_trigger_with_regex() {
    let trigger = Trigger::Event {
        event: "PRIVMSG".to_string(),
        target: None,
        regex: Some(r"^Hello, (\w+)!$".to_string()),
    };
    let msg = privmsg("#chan", "Hello, world!");
    let caps = check_trigger(&trigger, &msg, "bot").unwrap();
    assert_eq!(caps, vec!["world"]);
}

#[test]
fn event_trigger_regex_no_match() {
    let trigger = Trigger::Event {
        event: "PRIVMSG".to_string(),
        target: None,
        regex: Some(r"^Hello, (\w+)!$".to_string()),
    };
    let msg = privmsg("#chan", "Goodbye, world!");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

// ─── check_trigger: Mention ──────────────────────────────────────────────────

#[test]
fn mention_trigger_colon_separator() {
    let trigger = Trigger::Mention { target: None };
    let msg = privmsg("#chan", "rustbot: hello there");
    let caps = check_trigger(&trigger, &msg, "rustbot").unwrap();
    assert_eq!(caps, vec!["hello there"]);
}

#[test]
fn mention_trigger_comma_separator() {
    let trigger = Trigger::Mention { target: None };
    let msg = privmsg("#chan", "rustbot, what time is it?");
    let caps = check_trigger(&trigger, &msg, "rustbot").unwrap();
    assert_eq!(caps, vec!["what time is it?"]);
}

#[test]
fn mention_trigger_case_insensitive_nick() {
    let trigger = Trigger::Mention { target: None };
    assert!(check_trigger(&trigger, &privmsg("#chan", "RUSTBOT: hi"), "rustbot").is_some());
    assert!(check_trigger(&trigger, &privmsg("#chan", "RustBot: hi"), "rustbot").is_some());
}

#[test]
fn mention_trigger_wrong_nick() {
    let trigger = Trigger::Mention { target: None };
    let msg = privmsg("#chan", "otherbot: hello");
    assert!(check_trigger(&trigger, &msg, "rustbot").is_none());
}

#[test]
fn mention_trigger_no_separator() {
    // "rustbot hello" without a separator should NOT match.
    let trigger = Trigger::Mention { target: None };
    let msg = privmsg("#chan", "rustbot hello");
    assert!(check_trigger(&trigger, &msg, "rustbot").is_none());
}

#[test]
fn mention_trigger_ignores_non_privmsg() {
    let trigger = Trigger::Mention { target: None };
    let msg = ":nick!u@h JOIN #chan".parse().unwrap();
    assert!(check_trigger(&trigger, &msg, "rustbot").is_none());
}

#[test]
fn mention_trigger_target_filter() {
    let trigger = Trigger::Mention {
        target: Some("#rust".to_string()),
    };
    assert!(check_trigger(&trigger, &privmsg("#rust", "rustbot: hi"), "rustbot").is_some());
    assert!(check_trigger(&trigger, &privmsg("#other", "rustbot: hi"), "rustbot").is_none());
}

#[test]
fn mention_trigger_empty_rest() {
    // "rustbot: " — only whitespace after the separator: after trimming the
    // remainder is empty, so the captures vector should be empty too.
    let trigger = Trigger::Mention { target: None };
    let msg = privmsg("#chan", "rustbot: ");
    let caps = check_trigger(&trigger, &msg, "rustbot").unwrap();
    assert!(caps.is_empty());
}

// ─── glob_match: ? wildcard and literal dot ───────────────────────────────────

#[test]
fn glob_question_mark_matches_single_char() {
    assert!(glob_match("hel?o", "hello").is_some());
    assert!(glob_match("hel?o", "helXo").is_some());
}

#[test]
fn glob_question_mark_does_not_match_zero_chars() {
    // '?' must match exactly one character, so "hel?o" does not match "helo".
    assert!(glob_match("hel?o", "helo").is_none());
}

#[test]
fn glob_question_mark_does_not_match_two_chars() {
    assert!(glob_match("hel?o", "helllo").is_none());
}

#[test]
fn glob_literal_dot_matches_dot() {
    assert!(glob_match("3.14", "3.14").is_some());
}

#[test]
fn glob_literal_dot_does_not_match_any_char() {
    // '.' is NOT a regex wildcard in the glob pattern; it must only match a literal '.'.
    assert!(glob_match("3.14", "3X14").is_none());
}

// ─── check_trigger: Cron ────────────────────────────────────────────────────

#[test]
fn cron_trigger_never_matches_privmsg() {
    let trigger = Trigger::Cron {
        interval: std::time::Duration::from_secs(60),
        target: None,
    };
    let msg = privmsg("#chan", "hello");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn cron_trigger_never_matches_join() {
    let trigger = Trigger::Cron {
        interval: std::time::Duration::from_secs(60),
        target: Some("#chan".to_string()),
    };
    let msg = ":nick!u@h JOIN #chan".parse().unwrap();
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn event_trigger_target_filter() {
    let trigger = Trigger::Event {
        event: "JOIN".to_string(),
        target: Some("#rust".to_string()),
        regex: None,
    };
    let join_rust = ":nick!u@h JOIN #rust".parse().unwrap();
    let join_other = ":nick!u@h JOIN #other".parse().unwrap();
    assert!(check_trigger(&trigger, &join_rust, "bot").is_some());
    assert!(check_trigger(&trigger, &join_other, "bot").is_none());
}

#[test]
fn event_trigger_case_insensitive_event_name() {
    // The event field in the trigger is matched case-insensitively against
    // the command name.
    let trigger = Trigger::Event {
        event: "join".to_string(),
        target: None,
        regex: None,
    };
    let msg = ":nick!u@h JOIN #chan".parse().unwrap();
    assert!(check_trigger(&trigger, &msg, "bot").is_some());
}

#[test]
fn event_trigger_invalid_regex_returns_none() {
    let trigger = Trigger::Event {
        event: "PRIVMSG".to_string(),
        target: None,
        regex: Some("[invalid".to_string()),
    };
    let msg = privmsg("#chan", "hello");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}
