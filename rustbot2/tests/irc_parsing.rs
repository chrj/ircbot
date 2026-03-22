use irc_proto::chan::ChannelExt;
use irc_proto::Message;
use rustbot2::irc::MessageExt;
use rustbot2::CtcpMessage;

// ─── MessageExt helpers ───────────────────────────────────────────────────────

#[test]
fn trailing_privmsg() {
    let msg: Message = ":alice!a@host PRIVMSG #general :Hello, world!"
        .parse()
        .unwrap();
    assert_eq!(msg.trailing(), Some("Hello, world!"));
}

#[test]
fn trailing_ping() {
    let msg: Message = "PING :server.example.com".parse().unwrap();
    assert_eq!(msg.trailing(), Some("server.example.com"));
}

#[test]
fn trailing_join() {
    let msg: Message = ":carol!c@host JOIN #rust".parse().unwrap();
    assert_eq!(msg.trailing(), Some("#rust"));
}

#[test]
fn trailing_numeric() {
    let msg: Message = ":irc.server.net 001 mynick :Welcome to the network!"
        .parse()
        .unwrap();
    assert_eq!(msg.trailing(), Some("Welcome to the network!"));
}

#[test]
fn target_privmsg_channel() {
    let msg: Message = ":alice!a@host PRIVMSG #general :Hello, world!"
        .parse()
        .unwrap();
    assert_eq!(msg.target(), Some("#general"));
}

#[test]
fn target_privmsg_query() {
    let msg: Message = ":bob!b@host PRIVMSG botname :hey there".parse().unwrap();
    assert_eq!(msg.target(), Some("botname"));
}

#[test]
fn target_join() {
    let msg: Message = ":carol!c@host JOIN #rust".parse().unwrap();
    assert_eq!(msg.target(), Some("#rust"));
}

#[test]
fn nick_from_user_prefix() {
    let msg: Message = ":alice!a@host PRIVMSG #general :Hello, world!"
        .parse()
        .unwrap();
    assert_eq!(msg.nick(), Some("alice"));
}

#[test]
fn nick_from_server_prefix_returns_none() {
    // source_nickname() returns None for server prefixes.
    let msg: Message = ":irc.net 001 bot :Welcome".parse().unwrap();
    assert_eq!(msg.nick(), None);
}

#[test]
fn parse_user_full_prefix() {
    let msg: Message = ":nick!user@host PART #chan".parse().unwrap();
    let user = msg.parse_user().unwrap();
    assert_eq!(user.nick, "nick");
    assert_eq!(user.user, "user");
    assert_eq!(user.host, "host");
}

#[test]
fn parse_user_server_prefix_returns_none() {
    // Server prefixes have no '!user' component so parse_user returns None.
    let msg: Message = ":irc.net 001 bot :Welcome".parse().unwrap();
    assert!(msg.parse_user().is_none());
}

#[test]
fn command_str_privmsg() {
    let msg: Message = ":alice!a@host PRIVMSG #general :Hello, world!"
        .parse()
        .unwrap();
    assert_eq!(msg.command_str().as_ref(), "PRIVMSG");
}

#[test]
fn command_str_numeric() {
    let msg: Message = ":irc.server.net 001 mynick :Welcome to the network!"
        .parse()
        .unwrap();
    assert_eq!(msg.command_str().as_ref(), "001");
}

#[test]
fn command_str_is_uppercase() {
    // irc-proto normalises command names; verify our helper does too.
    let msg: Message = "PING :server".parse().unwrap();
    assert_eq!(msg.command_str().as_ref(), "PING");
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
    let msg: Message = ":alice!a@host PRIVMSG mybot :\x01VERSION\x01"
        .parse()
        .unwrap();
    let ctcp = CtcpMessage::parse(msg.trailing().unwrap()).unwrap();
    assert_eq!(ctcp.command, "VERSION");
}

// ─── ChannelExt::is_channel_name ──────────────────────────────────────────────

#[test]
fn is_channel_hash() {
    assert!("#general".is_channel_name());
}

#[test]
fn is_channel_ampersand() {
    assert!("&local".is_channel_name());
}

#[test]
fn is_channel_plus() {
    assert!("+moderated".is_channel_name());
}

#[test]
fn is_channel_bang() {
    assert!("!unique".is_channel_name());
}

#[test]
fn is_channel_nick_is_not_channel() {
    assert!(!"alice".is_channel_name());
}

#[test]
fn is_channel_empty_is_not_channel() {
    assert!(!"".is_channel_name());
}
