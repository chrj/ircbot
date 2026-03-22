//! IRC protocol types — backed by the [`irc-proto`](irc_proto) crate.
//!
//! This module re-exports the types from `irc-proto` that the rest of the
//! crate uses.

pub use irc_proto::chan::ChannelExt;
pub use irc_proto::command::Command;
pub use irc_proto::message::Message;
pub use irc_proto::prefix::Prefix;
pub use irc_proto::response::Response;

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
