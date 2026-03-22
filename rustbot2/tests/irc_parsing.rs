use irc_proto::chan::ChannelExt;
use irc_proto::prefix::Prefix;
use irc_proto::Message;
use rustbot2::CtcpMessage;

// ─── source_nickname ─────────────────────────────────────────────────────────

#[test]
fn nick_from_user_prefix() {
    let msg: Message = ":alice!a@host PRIVMSG #general :Hello, world!"
        .parse()
        .unwrap();
    assert_eq!(msg.source_nickname(), Some("alice"));
}

#[test]
fn nick_from_server_prefix_returns_none() {
    // source_nickname() returns None for server prefixes.
    let msg: Message = ":irc.net 001 bot :Welcome".parse().unwrap();
    assert_eq!(msg.source_nickname(), None);
}

// ─── prefix / Prefix matching ─────────────────────────────────────────────────

#[test]
fn prefix_full_nickname() {
    let msg: Message = ":nick!user@host PART #chan".parse().unwrap();
    match msg.prefix.as_ref().unwrap() {
        Prefix::Nickname(nick, user, host) => {
            assert_eq!(nick, "nick");
            assert_eq!(user, "user");
            assert_eq!(host, "host");
        }
        other => panic!("unexpected prefix: {other:?}"),
    }
}

#[test]
fn prefix_server_is_not_nickname() {
    let msg: Message = ":irc.net 001 bot :Welcome".parse().unwrap();
    assert!(
        !matches!(msg.prefix.as_ref().unwrap(), Prefix::Nickname(_, _, _)),
        "server prefix should not be a Prefix::Nickname"
    );
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
    // Extract the PRIVMSG text directly from the Command enum.
    let irc_proto::Command::PRIVMSG(_, text) = &msg.command else {
        panic!("expected PRIVMSG");
    };
    let ctcp = CtcpMessage::parse(text).unwrap();
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
