//! IRC protocol types — backed by the [`irc-proto`](irc_proto) crate.
//!
//! This module re-exports the types from `irc-proto` that the rest of the
//! crate uses, and provides a small extension trait ([`MessageExt`]) that
//! adds convenience methods on top of [`Message`].

pub use irc_proto::chan::ChannelExt;
pub use irc_proto::command::Command;
pub use irc_proto::message::Message;
pub use irc_proto::prefix::Prefix;
pub use irc_proto::response::Response;

/// Extension methods for [`Message`] that are not part of the upstream API.
pub trait MessageExt {
    /// The IRC command name as an uppercase ASCII string.
    ///
    /// Examples: `"PRIVMSG"`, `"JOIN"`, `"001"`.
    fn command_str(&self) -> std::borrow::Cow<'_, str>;

    /// The nick portion of the message prefix, if any.
    ///
    /// Returns `None` for server prefixes or when there is no prefix.
    fn nick(&self) -> Option<&str>;

    /// The trailing parameter — the main text content of the message.
    fn trailing(&self) -> Option<&str>;

    /// The first parameter — typically the target channel or nick.
    fn target(&self) -> Option<&str>;

    /// Parse the prefix into a [`crate::context::User`].
    ///
    /// Returns `None` when the prefix is absent, is a server name, or does
    /// not include a username (i.e. has no `!user` component).
    fn parse_user(&self) -> Option<crate::context::User>;
}

impl MessageExt for Message {
    fn command_str(&self) -> std::borrow::Cow<'_, str> {
        use std::borrow::Cow;
        match &self.command {
            // For Raw commands the name is already a plain string; borrow it.
            Command::Raw(name, _) => Cow::Borrowed(name.as_str()),
            // For every other variant, convert to the wire-format string and
            // take the first word (the command name).
            cmd => {
                let s = String::from(cmd);
                let end = s.find(' ').unwrap_or(s.len());
                Cow::Owned(s[..end].to_ascii_uppercase())
            }
        }
    }

    fn nick(&self) -> Option<&str> {
        self.source_nickname()
    }

    fn trailing(&self) -> Option<&str> {
        match &self.command {
            Command::PRIVMSG(_, text) | Command::NOTICE(_, text) => Some(text),
            Command::PING(server, _) => Some(server),
            Command::PONG(_, Some(token)) => Some(token),
            Command::PONG(server, None) => Some(server),
            Command::JOIN(channel, _, _) => Some(channel),
            Command::PART(_, Some(reason)) => Some(reason),
            Command::PART(channel, None) => Some(channel),
            Command::QUIT(Some(message)) => Some(message),
            Command::KICK(_, _, Some(reason)) => Some(reason),
            Command::TOPIC(_, Some(topic)) => Some(topic),
            Command::TOPIC(channel, None) => Some(channel),
            Command::Response(_, args) => args.last().map(String::as_str),
            Command::Raw(_, args) => args.last().map(String::as_str),
            _ => None,
        }
    }

    fn target(&self) -> Option<&str> {
        match &self.command {
            Command::PRIVMSG(target, _) | Command::NOTICE(target, _) => Some(target),
            Command::JOIN(channel, _, _) => Some(channel),
            Command::PART(channel, _) => Some(channel),
            Command::KICK(channel, _, _) => Some(channel),
            Command::TOPIC(channel, _) => Some(channel),
            Command::INVITE(_, channel) => Some(channel),
            Command::ChannelMODE(channel, _) => Some(channel),
            Command::UserMODE(nick, _) => Some(nick),
            Command::Response(_, args) => args.first().map(String::as_str),
            Command::Raw(_, args) => args.first().map(String::as_str),
            _ => None,
        }
    }

    fn parse_user(&self) -> Option<crate::context::User> {
        match self.prefix.as_ref()? {
            // Only return a User when there is a non-empty username component
            // (i.e. the prefix contains '!user'), matching the original
            // behaviour where split_once('!') returned None for bare nicks.
            Prefix::Nickname(nick, user, host) if !user.is_empty() => Some(crate::context::User {
                nick: nick.clone(),
                user: user.clone(),
                host: host.clone(),
            }),
            _ => None,
        }
    }
}

/// A parsed CTCP (Client-to-Client Protocol) message extracted from the
/// trailing parameter of a `PRIVMSG` or `NOTICE`.
///
/// CTCP messages are delimited by `\x01` bytes:
/// `\x01COMMAND [optional args]\x01`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtcpMessage {
    /// The CTCP command in uppercase (e.g. `"PING"`, `"VERSION"`, `"ACTION"`).
    pub command: String,
    /// Optional argument following the command (empty string when absent).
    pub arg: String,
}

impl CtcpMessage {
    /// Try to parse `text` (the trailing parameter of a `PRIVMSG`/`NOTICE`)
    /// as a CTCP message.  Returns `None` if `text` does not start with
    /// `\x01`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('\x01')?;
        // The closing \x01 is optional in some clients.
        let text = text.strip_suffix('\x01').unwrap_or(text);
        let (command, arg) = text.split_once(' ').unwrap_or((text, ""));
        Some(CtcpMessage {
            command: command.to_ascii_uppercase(),
            arg: arg.to_string(),
        })
    }
}
