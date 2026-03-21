use std::time::Duration;

use crate::irc::is_channel_name;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;

/// Default interval between client-initiated keepalive pings.
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// Default time to wait for a pong before treating the connection as dead.
pub const DEFAULT_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Holds the established connection to an IRC server plus join-on-connect metadata.
pub struct BotState {
    pub nick: String,
    pub channels: Vec<String>,
    /// Server address used when reconnecting (e.g. `"irc.example.net:6667"`).
    pub server: String,
    pub(crate) keepalive_interval: Duration,
    pub(crate) keepalive_timeout: Duration,
    pub(crate) reader: tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>,
    /// The raw write half; `run_bot_internal` wraps this in a buffered writer and a
    /// dedicated write-loop task.
    pub(crate) write_half: tokio::net::tcp::OwnedWriteHalf,
}

impl BotState {
    /// Normalise a channel name: if it doesn't start with a recognised IRC
    /// channel prefix (`#`, `&`, `+`, `!`) a `#` is prepended automatically.
    fn normalise_channel(ch: String) -> String {
        if is_channel_name(&ch) {
            ch
        } else {
            format!("#{ch}")
        }
    }

    /// Connect to an IRC server, send NICK/USER, and return a `BotState` ready to run.
    ///
    /// Channel names that do not already start with a channel prefix character
    /// (`#`, `&`, `+`, `!`) will automatically be prefixed with `#`, so both
    /// `"general"` and `"#general"` are accepted.
    ///
    /// # Errors
    ///
    /// Returns an error if the TCP connection or initial handshake fails.
    pub async fn connect(
        nick: String,
        server: &str,
        channels: Vec<String>,
    ) -> Result<BotState, Box<dyn std::error::Error + Send + Sync>> {
        let channels: Vec<String> = channels.into_iter().map(Self::normalise_channel).collect();
        let stream = TcpStream::connect(server).await?;
        let (read_half, write_half) = stream.into_split();
        let reader = tokio::io::BufReader::new(read_half);
        let mut writer = BufWriter::new(write_half);

        writer
            .write_all(format!("NICK {nick}\r\n").as_bytes())
            .await?;
        writer
            .write_all(format!("USER {nick} 0 * :{nick}\r\n").as_bytes())
            .await?;
        writer.flush().await?;

        // Recover the inner write half from the BufWriter.
        let write_half = writer.into_inner();

        Ok(BotState {
            nick,
            channels,
            server: server.to_string(),
            keepalive_interval: DEFAULT_KEEPALIVE_INTERVAL,
            keepalive_timeout: DEFAULT_KEEPALIVE_TIMEOUT,
            reader,
            write_half,
        })
    }

    /// Override the keepalive ping interval and pong timeout.
    ///
    /// By default the bot sends a `PING` every 30 seconds and waits 10 seconds
    /// for the corresponding `PONG` before treating the connection as dead and
    /// triggering a reconnect.  Call this method (before starting the bot) to
    /// use different values.
    pub fn with_keepalive(mut self, interval: Duration, timeout: Duration) -> Self {
        self.keepalive_interval = interval;
        self.keepalive_timeout = timeout;
        self
    }
}
