use crate::irc::IrcMessage;
use tokio::sync::mpsc::UnboundedSender;

/// A user on IRC (nick!user@host).
#[derive(Debug, Clone, Default)]
pub struct User {
    pub nick: String,
    pub user: String,
    pub host: String,
}

/// Per-message context passed to every handler.
pub struct Context {
    pub(crate) tx: UnboundedSender<String>,
    /// The channel or nick this message was directed to.
    pub target: String,
    pub is_channel: bool,
    /// The user who sent the message (if available).
    pub sender: Option<User>,
    /// The underlying parsed IRC message.
    pub raw: IrcMessage,
    /// The bot's own nick (for self-detection).
    pub bot_nick: String,
    /// Wildcard or regex captures from the matched trigger pattern.
    pub captures: Vec<String>,
}

/// Strip characters that could be used for IRC message injection.
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|&c| c != '\r' && c != '\n' && c != '\0')
        .collect()
}

impl Context {
    /// Reply to the sender.  In a channel, prefixes the nick; in a query, PMs back.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn reply(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        let raw = if self.is_channel {
            let prefix = self
                .sender
                .as_ref()
                .map(|u| format!("{}, ", u.nick))
                .unwrap_or_default();
            format!("PRIVMSG {} :{prefix}{msg}\r\n", self.target)
        } else {
            let to = self
                .sender
                .as_ref()
                .map_or(self.target.as_str(), |u| u.nick.as_str());
            format!("PRIVMSG {to} :{msg}\r\n")
        };
        self.tx
            .send(raw)
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// Send a message to the channel / private target without a nick prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn say(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        let target = if self.is_channel {
            self.target.clone()
        } else {
            self.sender
                .as_ref()
                .map_or_else(|| self.target.clone(), |u| u.nick.clone())
        };
        self.tx
            .send(format!("PRIVMSG {target} :{msg}\r\n"))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// Send a `/me` action.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn action(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        let target = if self.is_channel {
            self.target.clone()
        } else {
            self.sender
                .as_ref()
                .map_or_else(|| self.target.clone(), |u| u.nick.clone())
        };
        self.tx
            .send(format!("PRIVMSG {target} :\x01ACTION {msg}\x01\r\n"))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// The trailing text of the underlying IRC message.
    #[must_use]
    pub fn message_text(&self) -> &str {
        self.raw.trailing().unwrap_or("")
    }

    /// Send an IRC NOTICE to the channel / private target.
    ///
    /// NOTICEs are typically displayed without triggering audible alerts and
    /// must never be replied to automatically (by convention), making them
    /// suitable for bot status messages or one-shot notifications.
    pub async fn notice(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        let target = if self.is_channel {
            self.target.clone()
        } else {
            self.sender
                .as_ref()
                .map(|u| u.nick.clone())
                .unwrap_or_else(|| self.target.clone())
        };
        self.tx
            .send(format!("NOTICE {} :{}\r\n", target, msg))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// Send a private message directly to the sender, regardless of whether
    /// the original message arrived in a channel or a query window.
    ///
    /// Useful for sending sensitive or verbose information out of a public
    /// channel without flooding it.
    pub async fn whisper(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        let to = self
            .sender
            .as_ref()
            .map(|u| u.nick.as_str())
            .unwrap_or(self.target.as_str());
        self.tx
            .send(format!("PRIVMSG {} :{}\r\n", to, msg))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }
}
