use irc_proto::Message;
use ircbot::CtcpMessage;

// ─── CTCP parsing ─────────────────────────────────────────────────────────────

#[test]
fn ctcp_parse_version() {
    let ctcp = CtcpMessage::parse("\x01VERSION\x01").unwrap();
    assert_eq!(ctcp.command, "VERSION");
    assert_eq!(ctcp.arg, "");
}

#[test]
fn ctcp_parse_ping_with_token() {
    let ctcp = CtcpMessage::parse("\x01PING 1234567890\x01").unwrap();
    assert_eq!(ctcp.command, "PING");
    assert_eq!(ctcp.arg, "1234567890");
}

#[test]
fn ctcp_parse_action() {
    let ctcp = CtcpMessage::parse("\x01ACTION waves hello\x01").unwrap();
    assert_eq!(ctcp.command, "ACTION");
    assert_eq!(ctcp.arg, "waves hello");
}

#[test]
fn ctcp_parse_no_closing_delimiter() {
    // Some clients omit the trailing \x01.
    let ctcp = CtcpMessage::parse("\x01PING 42").unwrap();
    assert_eq!(ctcp.command, "PING");
    assert_eq!(ctcp.arg, "42");
}

#[test]
fn ctcp_parse_command_is_uppercase() {
    let ctcp = CtcpMessage::parse("\x01version\x01").unwrap();
    assert_eq!(ctcp.command, "VERSION");
}

#[test]
fn ctcp_parse_non_ctcp_returns_none() {
    assert!(CtcpMessage::parse("Hello, world!").is_none());
    assert!(CtcpMessage::parse("").is_none());
}

#[test]
fn ctcp_embedded_in_privmsg() {
    let msg: Message = ":alice!a@host PRIVMSG mybot :\x01VERSION\x01"
        .parse()
        .unwrap();
    // Extract the PRIVMSG text directly from the Command enum.
    let irc_proto::Command::PRIVMSG(_, text) = &msg.command else {
        panic!("expected PRIVMSG");
    };
    let ctcp = CtcpMessage::parse(text).unwrap();
    assert_eq!(ctcp.command, "VERSION");
}
