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
    s.chars().filter(|&c| c != '\r' && c != '\n').collect()
}

impl Context {
    /// Reply to the sender.  In a channel, prefixes the nick; in a query, PMs back.
    pub async fn reply(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        let raw = if self.is_channel {
            let prefix = self
                .sender
                .as_ref()
                .map(|u| format!("{}, ", u.nick))
                .unwrap_or_default();
            format!("PRIVMSG {} :{}{}\r\n", self.target, prefix, msg)
        } else {
            let to = self
                .sender
                .as_ref()
                .map(|u| u.nick.as_str())
                .unwrap_or(self.target.as_str());
            format!("PRIVMSG {} :{}\r\n", to, msg)
        };
        self.tx
            .send(raw)
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// Send a message to the channel / private target without a nick prefix.
    pub async fn say(&self, msg: impl std::fmt::Display) -> crate::Result {
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
            .send(format!("PRIVMSG {} :{}\r\n", target, msg))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// Send a `/me` action.
    pub async fn action(&self, msg: impl std::fmt::Display) -> crate::Result {
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
            .send(format!("PRIVMSG {} :\x01ACTION {}\x01\r\n", target, msg))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// The trailing text of the underlying IRC message.
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
