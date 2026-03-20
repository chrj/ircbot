use crate::irc::is_channel_name;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;

/// Holds the established connection to an IRC server plus join-on-connect metadata.
pub struct BotState {
    pub nick: String,
    pub channels: Vec<String>,
    pub(crate) reader: tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>,
    /// The raw write half; `run_bot_internal` wraps this in a buffered writer and a
    /// dedicated write-loop task.
    pub(crate) write_half: tokio::net::tcp::OwnedWriteHalf,
}

impl BotState {
    /// Normalise a channel name: if it doesn't start with a recognised IRC
    /// channel prefix (`#`, `&`, `+`, `!`) a `#` is prepended automatically.
    fn normalise_channel(ch: String) -> String {
        if is_channel_name(&ch) { ch } else { format!("#{}", ch) }
    }

    /// Connect to an IRC server, send NICK/USER, and return a `BotState` ready to run.
    ///
    /// Channel names that do not already start with a channel prefix character
    /// (`#`, `&`, `+`, `!`) will automatically be prefixed with `#`, so both
    /// `"general"` and `"#general"` are accepted.
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
            .write_all(format!("NICK {}\r\n", nick).as_bytes())
            .await?;
        writer
            .write_all(format!("USER {} 0 * :{}\r\n", nick, nick).as_bytes())
            .await?;
        writer.flush().await?;

        // Recover the inner write half from the BufWriter.
        let write_half = writer.into_inner();

        Ok(BotState { nick, channels, reader, write_half })
    }
}
