use rustbot2::irc::IrcMessage;
use rustbot2::CtcpMessage;

#[test]
fn parse_ping() {
    let msg = IrcMessage::parse("PING :server.example.com").unwrap();
    assert_eq!(msg.command, "PING");
    assert_eq!(msg.prefix, None);
    assert_eq!(msg.params, vec!["server.example.com"]);
    assert_eq!(msg.trailing(), Some("server.example.com"));
}

#[test]
fn parse_privmsg_channel() {
    let msg = IrcMessage::parse(":alice!a@host PRIVMSG #general :Hello, world!").unwrap();
    assert_eq!(msg.command, "PRIVMSG");
    assert_eq!(msg.prefix, Some("alice!a@host".to_string()));
    assert_eq!(msg.params, vec!["#general", "Hello, world!"]);
    assert_eq!(msg.nick(), Some("alice"));
    assert_eq!(msg.target(), Some("#general"));
    assert_eq!(msg.trailing(), Some("Hello, world!"));
}

#[test]
fn parse_privmsg_query() {
    let msg = IrcMessage::parse(":bob!b@host PRIVMSG botname :hey there").unwrap();
    assert_eq!(msg.target(), Some("botname"));
    assert_eq!(msg.trailing(), Some("hey there"));
}

#[test]
fn parse_join() {
    let msg = IrcMessage::parse(":carol!c@host JOIN #rust").unwrap();
    assert_eq!(msg.command, "JOIN");
    assert_eq!(msg.nick(), Some("carol"));
    assert_eq!(msg.target(), Some("#rust"));
}

#[test]
fn parse_numeric() {
    let msg = IrcMessage::parse(":irc.server.net 001 mynick :Welcome to the network!").unwrap();
    assert_eq!(msg.command, "001");
    assert_eq!(msg.params[0], "mynick");
    assert_eq!(msg.trailing(), Some("Welcome to the network!"));
}

#[test]
fn parse_crlf_stripped() {
    let msg = IrcMessage::parse("PING :server\r\n").unwrap();
    assert_eq!(msg.command, "PING");
    assert_eq!(msg.trailing(), Some("server"));
}

#[test]
fn parse_no_trailing() {
    let msg = IrcMessage::parse(":server.net MODE #chan +o alice").unwrap();
    assert_eq!(msg.command, "MODE");
    assert_eq!(msg.params, vec!["#chan", "+o", "alice"]);
}

#[test]
fn parse_user_prefix() {
    let msg = IrcMessage::parse(":nick!user@host PART #chan").unwrap();
    let user = msg.parse_user().unwrap();
    assert_eq!(user.nick, "nick");
    assert_eq!(user.user, "user");
    assert_eq!(user.host, "host");
}

#[test]
fn parse_server_prefix_no_user() {
    // Server prefixes have no '!' so parse_user returns None.
    let msg = IrcMessage::parse(":irc.net 001 bot :Welcome").unwrap();
    assert!(msg.parse_user().is_none());
}

#[test]
fn parse_empty_returns_none() {
    assert!(IrcMessage::parse("").is_none());
    assert!(IrcMessage::parse("\r\n").is_none());
}

#[test]
fn command_is_uppercase() {
    let msg = IrcMessage::parse("ping :server").unwrap();
    assert_eq!(msg.command, "PING");
}

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
    let msg =
        IrcMessage::parse(":alice!a@host PRIVMSG mybot :\x01VERSION\x01").unwrap();
    let ctcp = CtcpMessage::parse(msg.trailing().unwrap()).unwrap();
    assert_eq!(ctcp.command, "VERSION");
}
