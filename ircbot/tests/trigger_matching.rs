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
        role: None,
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
        role: None,
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
        role: None,
    };
    let msg = privmsg("#chan", "!pong");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn command_trigger_target_match() {
    let trigger = Trigger::Command {
        name: "hi".to_string(),
        target: Some("#general".to_string()),
        role: None,
    };
    assert!(check_trigger(&trigger, &privmsg("#general", "!hi"), "bot").is_some());
    assert!(check_trigger(&trigger, &privmsg("#other", "!hi"), "bot").is_none());
}

#[test]
fn command_trigger_case_insensitive_name() {
    let trigger = Trigger::Command {
        name: "Ping".to_string(),
        target: None,
        role: None,
    };
    assert!(check_trigger(&trigger, &privmsg("#chan", "!ping"), "bot").is_some());
    assert!(check_trigger(&trigger, &privmsg("#chan", "!PING"), "bot").is_some());
}

#[test]
fn command_trigger_ignores_non_privmsg() {
    let trigger = Trigger::Command {
        name: "ping".to_string(),
        target: None,
        role: None,
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
        schedule: "0 0 * * * *".to_string(),
        tz: "UTC".to_string(),
        target: None,
    };
    let msg = privmsg("#chan", "hello");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn cron_trigger_never_matches_join() {
    let trigger = Trigger::Cron {
        schedule: "0 0 8-16 * * MON-FRI".to_string(),
        tz: "UTC".to_string(),
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

// ─── check_trigger: Event over other command kinds ────────────────────────────
//
// These exercise the `command_name`, `trailing_param` and `target_param`
// helpers for command variants beyond PRIVMSG/JOIN.

#[test]
fn event_notice_regex_matches_trailing_param() {
    let trigger = Trigger::Event {
        event: "NOTICE".to_string(),
        target: None,
        regex: Some(r"(\w+) back".to_string()),
    };
    let msg: Message = ":nick!u@h NOTICE #chan :ping back".parse().unwrap();
    let caps = check_trigger(&trigger, &msg, "bot").unwrap();
    assert_eq!(caps, vec!["ping"]);
}

#[test]
fn event_topic_target_filter_and_trailing_regex() {
    let trigger = Trigger::Event {
        event: "TOPIC".to_string(),
        target: Some("#chan".to_string()),
        regex: Some(r"new (.+)".to_string()),
    };
    let msg: Message = ":nick!u@h TOPIC #chan :new topic here".parse().unwrap();
    let caps = check_trigger(&trigger, &msg, "bot").unwrap();
    assert_eq!(caps, vec!["topic here"]);

    // Wrong channel is filtered out via target_param.
    let other: Message = ":nick!u@h TOPIC #other :new topic here".parse().unwrap();
    assert!(check_trigger(&trigger, &other, "bot").is_none());
}

#[test]
fn event_kick_target_filter_and_reason_regex() {
    let trigger = Trigger::Event {
        event: "KICK".to_string(),
        target: Some("#chan".to_string()),
        regex: Some(r"(spam)".to_string()),
    };
    let msg: Message = ":op!u@h KICK #chan baduser :spam".parse().unwrap();
    let caps = check_trigger(&trigger, &msg, "bot").unwrap();
    assert_eq!(caps, vec!["spam"]);
}

#[test]
fn event_invite_target_filter_uses_channel_param() {
    let trigger = Trigger::Event {
        event: "INVITE".to_string(),
        target: Some("#secret".to_string()),
        regex: None,
    };
    let msg: Message = ":nick!u@h INVITE testbot #secret".parse().unwrap();
    assert!(check_trigger(&trigger, &msg, "bot").is_some());

    let wrong: Message = ":nick!u@h INVITE testbot #public".parse().unwrap();
    assert!(check_trigger(&trigger, &wrong, "bot").is_none());
}

#[test]
fn event_raw_command_matches_name_target_and_trailing() {
    // An unknown command parses to `Command::Raw`, exercising the `Raw` arms of
    // command_name / target_param / trailing_param.
    let trigger = Trigger::Event {
        event: "FOOBAR".to_string(),
        target: Some("#chan".to_string()),
        regex: Some(r"hello (\w+)".to_string()),
    };
    let msg: Message = ":serv FOOBAR #chan :hello world".parse().unwrap();
    let caps = check_trigger(&trigger, &msg, "bot").unwrap();
    assert_eq!(caps, vec!["world"]);
}

// ─── check_trigger: CTCP messages never match text triggers ──────────────────

#[test]
fn event_privmsg_trigger_ignores_ctcp_action() {
    let trigger = Trigger::Event {
        event: "PRIVMSG".to_string(),
        target: None,
        regex: None,
    };
    let msg = privmsg("#chan", "\x01ACTION waves\x01");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn event_privmsg_trigger_ignores_ctcp_without_closing_byte() {
    // Some clients omit the closing \x01.
    let trigger = Trigger::Event {
        event: "PRIVMSG".to_string(),
        target: None,
        regex: None,
    };
    let msg = privmsg("#chan", "\x01ACTION waves");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn event_privmsg_regex_trigger_ignores_ctcp_action() {
    let trigger = Trigger::Event {
        event: "PRIVMSG".to_string(),
        target: None,
        regex: Some("(.*)".to_string()),
    };
    let msg = privmsg("#chan", "\x01ACTION waves\x01");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn command_trigger_ignores_ctcp_message() {
    let trigger = Trigger::Command {
        name: "ping".to_string(),
        target: None,
        role: None,
    };
    let msg = privmsg("#chan", "\x01!ping\x01");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn mention_trigger_ignores_ctcp_message() {
    let trigger = Trigger::Mention { target: None };
    let msg = privmsg("#chan", "\x01rustbot: hello\x01");
    assert!(check_trigger(&trigger, &msg, "rustbot").is_none());
}

#[test]
fn message_trigger_ignores_ctcp_action() {
    let trigger = Trigger::Message {
        pattern: "*".to_string(),
        target: None,
    };
    let msg = privmsg("#chan", "\x01ACTION waves\x01");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

// ─── check_trigger: Action ───────────────────────────────────────────────────

#[test]
fn action_trigger_glob_captures_wildcards() {
    let trigger = Trigger::Action {
        pattern: "slaps * around a bit with a large trout".to_string(),
        target: None,
    };
    let msg = privmsg(
        "#chan",
        "\x01ACTION slaps bob around a bit with a large trout\x01",
    );
    let caps = check_trigger(&trigger, &msg, "bot").unwrap();
    assert_eq!(caps, vec!["bob"]);
}

#[test]
fn action_trigger_wildcard_matches_any_action() {
    let trigger = Trigger::Action {
        pattern: "*".to_string(),
        target: None,
    };
    let caps = check_trigger(&trigger, &privmsg("#chan", "\x01ACTION waves\x01"), "bot").unwrap();
    assert_eq!(caps, vec!["waves"]);
}

#[test]
fn action_trigger_without_closing_byte() {
    let trigger = Trigger::Action {
        pattern: "*".to_string(),
        target: None,
    };
    let caps = check_trigger(&trigger, &privmsg("#chan", "\x01ACTION waves"), "bot").unwrap();
    assert_eq!(caps, vec!["waves"]);
}

#[test]
fn action_trigger_ignores_other_ctcp_commands() {
    let trigger = Trigger::Action {
        pattern: "*".to_string(),
        target: None,
    };
    let msg = privmsg("#chan", "\x01DCC SEND file 1 2 3\x01");
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn action_trigger_ignores_plain_privmsg() {
    let trigger = Trigger::Action {
        pattern: "*".to_string(),
        target: None,
    };
    assert!(check_trigger(&trigger, &privmsg("#chan", "waves"), "bot").is_none());
}

#[test]
fn action_trigger_ignores_notice() {
    let trigger = Trigger::Action {
        pattern: "*".to_string(),
        target: None,
    };
    let msg: Message = ":nick!u@h NOTICE #chan :\x01ACTION waves\x01"
        .parse()
        .unwrap();
    assert!(check_trigger(&trigger, &msg, "bot").is_none());
}

#[test]
fn action_trigger_target_filter() {
    let trigger = Trigger::Action {
        pattern: "*".to_string(),
        target: Some("#rust".to_string()),
    };
    let action = "\x01ACTION waves\x01";
    assert!(check_trigger(&trigger, &privmsg("#rust", action), "bot").is_some());
    assert!(check_trigger(&trigger, &privmsg("#other", action), "bot").is_none());
}

// ─── check_trigger: Ctcp ─────────────────────────────────────────────────────

#[test]
fn ctcp_trigger_captures_argument() {
    let trigger = Trigger::Ctcp {
        command: "DCC".to_string(),
        target: None,
    };
    let msg = privmsg("testbot", "\x01DCC SEND file 1 2 3\x01");
    let caps = check_trigger(&trigger, &msg, "testbot").unwrap();
    assert_eq!(caps, vec!["SEND file 1 2 3"]);
}

#[test]
fn ctcp_trigger_captures_empty_argument() {
    let trigger = Trigger::Ctcp {
        command: "TIME".to_string(),
        target: None,
    };
    let caps = check_trigger(&trigger, &privmsg("testbot", "\x01TIME\x01"), "testbot").unwrap();
    assert_eq!(caps, vec![""]);
}

#[test]
fn ctcp_trigger_case_insensitive_command() {
    let trigger = Trigger::Ctcp {
        command: "time".to_string(),
        target: None,
    };
    assert!(check_trigger(&trigger, &privmsg("testbot", "\x01TIME\x01"), "testbot").is_some());
}

#[test]
fn ctcp_trigger_wrong_command() {
    let trigger = Trigger::Ctcp {
        command: "TIME".to_string(),
        target: None,
    };
    let msg = privmsg("testbot", "\x01CLIENTINFO\x01");
    assert!(check_trigger(&trigger, &msg, "testbot").is_none());
}

#[test]
fn ctcp_trigger_matches_action() {
    let trigger = Trigger::Ctcp {
        command: "ACTION".to_string(),
        target: None,
    };
    let caps = check_trigger(&trigger, &privmsg("#chan", "\x01ACTION waves\x01"), "bot").unwrap();
    assert_eq!(caps, vec!["waves"]);
}

#[test]
fn ctcp_trigger_ignores_plain_privmsg() {
    let trigger = Trigger::Ctcp {
        command: "TIME".to_string(),
        target: None,
    };
    assert!(check_trigger(&trigger, &privmsg("testbot", "TIME"), "testbot").is_none());
}

#[test]
fn ctcp_trigger_target_filter() {
    let trigger = Trigger::Ctcp {
        command: "TIME".to_string(),
        target: Some("#rust".to_string()),
    };
    assert!(check_trigger(&trigger, &privmsg("#rust", "\x01TIME\x01"), "bot").is_some());
    assert!(check_trigger(&trigger, &privmsg("#other", "\x01TIME\x01"), "bot").is_none());
}

// ─── check_trigger: formatting codes ─────────────────────────────────────────

#[test]
fn check_trigger_matches_a_command_in_bold() {
    let trigger = Trigger::Command {
        name: "ping".to_string(),
        target: None,
        role: None,
    };
    let caps = check_trigger(&trigger, &privmsg("#chan", "\x02!ping\x02"), "bot");
    assert_eq!(caps, Some(vec![]));
}

#[test]
fn check_trigger_gives_a_capture_without_the_codes() {
    let trigger = Trigger::Message {
        pattern: "seen *".to_string(),
        target: None,
    };
    let caps = check_trigger(&trigger, &privmsg("#chan", "seen \x02bob\x02"), "bot");
    assert_eq!(caps, Some(vec!["bob".to_string()]));
}

#[test]
fn check_trigger_matches_a_mention_in_colour() {
    let trigger = Trigger::Mention { target: None };
    let caps = check_trigger(&trigger, &privmsg("#chan", "\x0304bot:\x03 hi"), "bot");
    assert_eq!(caps, Some(vec!["hi".to_string()]));
}

#[test]
fn check_trigger_gives_an_action_capture_without_the_codes() {
    let trigger = Trigger::Action {
        pattern: "waves at *".to_string(),
        target: None,
    };
    let msg = privmsg("#chan", "\x01ACTION waves at \x02bob\x02\x01");
    assert_eq!(
        check_trigger(&trigger, &msg, "bot"),
        Some(vec!["bob".to_string()])
    );
}

#[test]
fn check_trigger_keeps_the_codes_in_a_ctcp_payload() {
    // A CTCP payload is protocol data, so the capture carries it as it arrived.
    let trigger = Trigger::Ctcp {
        command: "DCC".to_string(),
        target: None,
    };
    let msg = privmsg("bot", "\x01DCC SEND \x02file\x02\x01");
    assert_eq!(
        check_trigger(&trigger, &msg, "bot"),
        Some(vec!["SEND \x02file\x02".to_string()])
    );
}

#[test]
fn check_trigger_matches_an_event_regex_without_the_codes() {
    let trigger = Trigger::Event {
        event: "TOPIC".to_string(),
        target: None,
        regex: Some("^release (.+)$".to_string()),
    };
    let msg: Message = ":nick!u@h TOPIC #chan :release \x021.2.3\x02"
        .parse()
        .unwrap();
    assert_eq!(
        check_trigger(&trigger, &msg, "bot"),
        Some(vec!["1.2.3".to_string()])
    );
}
