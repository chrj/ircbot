/// Returns `true` if `name` starts with an IRC channel prefix character
/// (`#`, `&`, `+`, `!`).
#[must_use]
pub fn is_channel_name(name: &str) -> bool {
    matches!(name.chars().next(), Some('#' | '&' | '+' | '!'))
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

/// A parsed IRC message.
#[derive(Debug, Clone)]
pub struct IrcMessage {
    /// Optional prefix (server or nick!user@host).
    pub prefix: Option<String>,
    /// IRC command in uppercase (e.g. "PRIVMSG", "PING", "001").
    pub command: String,
    /// Parameters, with the trailing parameter (after `:`) as the last element.
    pub params: Vec<String>,
}

impl IrcMessage {
    /// Parse a single IRC line.  Returns `None` on empty / malformed input.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            return None;
        }

        let mut pos = 0_usize;

        let prefix = if line.starts_with(':') {
            let end = line.find(' ')?;
            let pfx = line[1..end].to_string();
            pos = end + 1;
            Some(pfx)
        } else {
            None
        };

        let rest = &line[pos..];
        let cmd_end = rest.find(' ').unwrap_or(rest.len());
        let command = rest[..cmd_end].to_uppercase();

        let mut params: Vec<String> = Vec::new();
        let mut tail = if cmd_end < rest.len() {
            &rest[cmd_end + 1..]
        } else {
            ""
        };

        while !tail.is_empty() {
            if let Some(stripped) = tail.strip_prefix(':') {
                params.push(stripped.to_string());
                break;
            }
            let end = tail.find(' ').unwrap_or(tail.len());
            params.push(tail[..end].to_string());
            tail = if end < tail.len() {
                tail[end + 1..].trim_start_matches(' ')
            } else {
                ""
            };
        }

        Some(IrcMessage {
            prefix,
            command,
            params,
        })
    }

    /// Extracts the nick portion of the prefix (everything before `!`).
    #[must_use]
    pub fn nick(&self) -> Option<&str> {
        let prefix = self.prefix.as_deref()?;
        Some(prefix.split('!').next().unwrap_or(prefix))
    }

    /// The trailing parameter (last param, originally prefixed with `:`).
    pub fn trailing(&self) -> Option<&str> {
        self.params.last().map(String::as_str)
    }

    /// The first parameter — typically the target channel or nick.
    pub fn target(&self) -> Option<&str> {
        self.params.first().map(String::as_str)
    }

    /// Parse the prefix into a [`crate::context::User`].
    #[must_use]
    pub fn parse_user(&self) -> Option<crate::context::User> {
        let prefix = self.prefix.as_deref()?;
        let (nick_part, rest) = prefix.split_once('!')?;
        let (user_part, host_part) = rest.split_once('@').unwrap_or((rest, ""));
        Some(crate::context::User {
            nick: nick_part.to_string(),
            user: user_part.to_string(),
            host: host_part.to_string(),
        })
    }
}
