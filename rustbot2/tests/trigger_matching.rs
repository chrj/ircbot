use rustbot2::bot::{check_trigger, glob_match};
use rustbot2::handler::Trigger;
use rustbot2::irc::IrcMessage;

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

fn privmsg(target: &str, text: &str) -> IrcMessage {
    IrcMessage::parse(&format!(":nick!u@h PRIVMSG {} :{}", target, text)).unwrap()
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
    let msg = IrcMessage::parse(":nick!u@h JOIN #chan").unwrap();
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
    let msg = IrcMessage::parse(":nick!u@h JOIN #chan").unwrap();
    assert!(check_trigger(&trigger, &msg, "bot").is_some());
}

#[test]
fn event_trigger_join_wrong_event() {
    let trigger = Trigger::Event {
        event: "PART".to_string(),
        target: None,
        regex: None,
    };
    let msg = IrcMessage::parse(":nick!u@h JOIN #chan").unwrap();
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
