use std::time::Duration;

use irc_proto::chan::ChannelExt;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;

/// Default interval between client-initiated keepalive pings.
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// Default time to wait for a pong before treating the connection as dead.
pub const DEFAULT_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Default number of messages that may be sent in rapid succession before
/// rate-limiting kicks in (token-bucket burst size).
pub const DEFAULT_FLOOD_BURST: usize = 4;
/// Default minimum interval between messages once the burst budget is exhausted.
pub const DEFAULT_FLOOD_RATE: Duration = Duration::from_millis(500);

/// Holds the established connection to an IRC server plus join-on-connect metadata.
pub struct State {
    pub nick: String,
    pub channels: Vec<String>,
    /// Server address used when reconnecting (e.g. `"irc.example.net:6667"`).
    pub server: String,
    pub(crate) keepalive_interval: Duration,
    pub(crate) keepalive_timeout: Duration,
    /// Token-bucket burst: how many messages may be sent immediately before
    /// rate-limiting kicks in.
    pub(crate) flood_burst: usize,
    /// Minimum interval between messages once the burst budget is exhausted.
    pub(crate) flood_rate: Duration,
    pub(crate) reader: tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>,
    /// The raw write half; `run_bot_internal` wraps this in a buffered writer and a
    /// dedicated write-loop task.
    pub(crate) write_half: tokio::net::tcp::OwnedWriteHalf,
    /// The raw file descriptor of the underlying TCP socket.
    /// Used by the hot-reload path to pass the live connection to a new binary.
    #[cfg(unix)]
    pub raw_fd: std::os::unix::io::RawFd,
}

impl State {
    /// Normalise a channel name: if it doesn't start with a recognised IRC
    /// channel prefix (`#`, `&`, `+`, `!`) a `#` is prepended automatically.
    fn normalise_channel(ch: String) -> String {
        if ch.is_channel_name() {
            ch
        } else {
            format!("#{ch}")
        }
    }

    /// Connect to an IRC server, send NICK/USER, and return a `State` ready to run.
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
    ) -> Result<State, Box<dyn std::error::Error + Send + Sync>> {
        let channels: Vec<String> = channels.into_iter().map(Self::normalise_channel).collect();
        let stream = TcpStream::connect(server).await?;

        #[cfg(unix)]
        let raw_fd = {
            use std::os::unix::io::AsRawFd;
            stream.as_raw_fd()
        };

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

        Ok(State {
            nick,
            channels,
            server: server.to_string(),
            keepalive_interval: DEFAULT_KEEPALIVE_INTERVAL,
            keepalive_timeout: DEFAULT_KEEPALIVE_TIMEOUT,
            flood_burst: DEFAULT_FLOOD_BURST,
            flood_rate: DEFAULT_FLOOD_RATE,
            reader,
            write_half,
            #[cfg(unix)]
            raw_fd,
        })
    }

    /// Attempt to reconstruct a [`State`] from an inherited TCP file descriptor.
    ///
    /// When the bot is reloaded via [`crate::hot_reload::exec_reload`] the new
    /// binary inherits the live TCP socket.  This method reads the metadata
    /// from the environment variables written by `exec_reload` and wraps the
    /// raw fd in a Tokio `TcpStream` — no new TCP connection is made, so the
    /// IRC session is never interrupted.
    ///
    /// Returns `None` if the expected environment variables are absent (i.e.
    /// this is a fresh start, not a reload).
    ///
    /// # Errors
    ///
    /// Returns an error if the env vars are malformed or if the fd cannot be
    /// converted to a `TcpStream`.
    #[cfg(unix)]
    pub fn try_inherit_from_env() -> Result<Option<State>, Box<dyn std::error::Error + Send + Sync>>
    {
        use std::os::unix::io::{FromRawFd, RawFd};

        use crate::hot_reload::{
            ENV_CHANNELS, ENV_FD, ENV_KA_INTERVAL, ENV_KA_TIMEOUT, ENV_NICK, ENV_SERVER,
        };

        let fd_str = match std::env::var(ENV_FD) {
            Ok(v) => v,
            Err(_) => return Ok(None), // normal startup
        };

        let raw_fd: RawFd = fd_str.parse()?;
        let nick = std::env::var(ENV_NICK)?;
        let server = std::env::var(ENV_SERVER)?;
        let channels_raw = std::env::var(ENV_CHANNELS)?;
        let ka_interval_ms: u64 = std::env::var(ENV_KA_INTERVAL)?.parse()?;
        let ka_timeout_ms: u64 = std::env::var(ENV_KA_TIMEOUT)?.parse()?;

        // Clear the env vars so they are not accidentally inherited by any
        // child processes the bot might spawn.
        for var in &[
            ENV_FD,
            ENV_NICK,
            ENV_SERVER,
            ENV_CHANNELS,
            ENV_KA_INTERVAL,
            ENV_KA_TIMEOUT,
        ] {
            std::env::remove_var(var);
        }

        let channels: Vec<String> = if channels_raw.is_empty() {
            vec![]
        } else {
            channels_raw.split(',').map(str::to_string).collect()
        };

        // Reconstruct the TcpStream from the raw fd.  Safety: the fd was
        // inherited from the parent process and is still valid.
        let std_stream = unsafe { std::net::TcpStream::from_raw_fd(raw_fd) };
        std_stream.set_nonblocking(true)?;
        let stream = TcpStream::from_std(std_stream)?;

        let (read_half, write_half) = stream.into_split();
        let reader = tokio::io::BufReader::new(read_half);

        Ok(Some(State {
            nick,
            channels,
            server,
            keepalive_interval: Duration::from_millis(ka_interval_ms),
            keepalive_timeout: Duration::from_millis(ka_timeout_ms),
            flood_burst: DEFAULT_FLOOD_BURST,
            flood_rate: DEFAULT_FLOOD_RATE,
            reader,
            write_half,
            raw_fd,
        }))
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

    /// Override the flood-control token-bucket settings.
    ///
    /// `burst` is the number of messages that may be sent immediately before
    /// rate-limiting kicks in.  `rate` is the minimum interval between messages
    /// once the burst budget is exhausted.
    pub fn with_flood_control(mut self, burst: usize, rate: Duration) -> Self {
        self.flood_burst = burst;
        self.flood_rate = rate;
        self
    }

    /// Returns the configured keepalive interval.
    pub fn keepalive_interval(&self) -> Duration {
        self.keepalive_interval
    }

    /// Returns the configured keepalive timeout.
    pub fn keepalive_timeout(&self) -> Duration {
        self.keepalive_timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    // ── normalise_channel ──────────────────────────────────────────────────────

    #[test]
    fn normalise_channel_prefixes_bare_name() {
        assert_eq!(State::normalise_channel("general".to_string()), "#general");
    }

    #[test]
    fn normalise_channel_keeps_existing_prefixes() {
        for ch in ["#rust", "&local", "+modeless", "!network"] {
            assert_eq!(State::normalise_channel(ch.to_string()), ch);
        }
    }

    // ── builders / getters ─────────────────────────────────────────────────────

    /// Connect to an in-process loopback listener so a real `State` can be built
    /// without an external IRC server.
    async fn connect_loopback() -> State {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        // Accept (and hold) the connection so the NICK/USER handshake write
        // succeeds.
        tokio::spawn(async move {
            let _sock = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        State::connect("tester".to_string(), &addr, vec!["general".to_string()])
            .await
            .expect("loopback connect failed")
    }

    #[tokio::test]
    async fn connect_normalises_channels() {
        let state = connect_loopback().await;
        assert_eq!(state.channels, vec!["#general".to_string()]);
    }

    #[tokio::test]
    async fn with_keepalive_overrides_settings() {
        let state = connect_loopback()
            .await
            .with_keepalive(Duration::from_secs(7), Duration::from_secs(3));
        assert_eq!(state.keepalive_interval(), Duration::from_secs(7));
        assert_eq!(state.keepalive_timeout(), Duration::from_secs(3));
    }

    #[tokio::test]
    async fn with_flood_control_overrides_settings() {
        let state = connect_loopback()
            .await
            .with_flood_control(9, Duration::from_millis(250));
        assert_eq!(state.flood_burst, 9);
        assert_eq!(state.flood_rate, Duration::from_millis(250));
    }

    // ── try_inherit_from_env (unix) ────────────────────────────────────────────
    //
    // These tests mutate process-global environment variables, so they are
    // serialised behind a shared mutex to avoid racing each other.

    #[cfg(unix)]
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(unix)]
    fn clear_inherit_env() {
        use crate::hot_reload::{
            ENV_CHANNELS, ENV_FD, ENV_KA_INTERVAL, ENV_KA_TIMEOUT, ENV_NICK, ENV_SERVER,
        };
        for var in [
            ENV_FD,
            ENV_NICK,
            ENV_SERVER,
            ENV_CHANNELS,
            ENV_KA_INTERVAL,
            ENV_KA_TIMEOUT,
        ] {
            std::env::remove_var(var);
        }
    }

    #[cfg(unix)]
    #[test]
    fn try_inherit_returns_none_on_normal_startup() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_inherit_env();
        let result = State::try_inherit_from_env().expect("should not error");
        assert!(result.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn try_inherit_errors_on_malformed_fd() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_inherit_env();
        std::env::set_var(crate::hot_reload::ENV_FD, "notanint");
        let result = State::try_inherit_from_env();
        clear_inherit_env();
        assert!(result.is_err(), "malformed fd should yield an error");
    }

    /// Full happy path: a live loopback fd plus all metadata env vars is
    /// reconstructed into a `State` with the channels parsed and keepalive
    /// settings restored — the same path taken after `exec_reload`.
    #[cfg(unix)]
    #[tokio::test]
    async fn try_inherit_reconstructs_state_from_env() {
        use std::os::unix::io::IntoRawFd;

        use crate::hot_reload::{
            ENV_CHANNELS, ENV_FD, ENV_KA_INTERVAL, ENV_KA_TIMEOUT, ENV_NICK, ENV_SERVER,
        };

        // A real connected loopback socket whose fd we can inherit.  All async
        // setup happens *before* the env lock so the guard never spans an
        // `.await` (`try_inherit_from_env` itself is synchronous).
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _sock = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let std_stream = std::net::TcpStream::connect(&addr).expect("connect failed");
        let raw_fd = std_stream.into_raw_fd();

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_inherit_env();
        std::env::set_var(ENV_FD, raw_fd.to_string());
        std::env::set_var(ENV_NICK, "inheritbot");
        std::env::set_var(ENV_SERVER, &addr);
        std::env::set_var(ENV_CHANNELS, "#a,#b");
        std::env::set_var(ENV_KA_INTERVAL, "12000");
        std::env::set_var(ENV_KA_TIMEOUT, "4000");

        let state = State::try_inherit_from_env()
            .expect("inherit should succeed")
            .expect("env vars present → Some(State)");

        assert_eq!(state.nick, "inheritbot");
        assert_eq!(state.server, addr);
        assert_eq!(state.channels, vec!["#a".to_string(), "#b".to_string()]);
        assert_eq!(state.keepalive_interval(), Duration::from_millis(12000));
        assert_eq!(state.keepalive_timeout(), Duration::from_millis(4000));

        // try_inherit_from_env clears the env vars once consumed.
        assert!(std::env::var(ENV_FD).is_err());
    }

    /// An empty `IRCBOT_CHANNELS` must yield an empty channel list (not `[""]`).
    #[cfg(unix)]
    #[tokio::test]
    async fn try_inherit_parses_empty_channels() {
        use std::os::unix::io::IntoRawFd;

        use crate::hot_reload::{
            ENV_CHANNELS, ENV_FD, ENV_KA_INTERVAL, ENV_KA_TIMEOUT, ENV_NICK, ENV_SERVER,
        };

        // Async setup before the env lock (see sibling test for rationale).
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _sock = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let raw_fd = std::net::TcpStream::connect(&addr)
            .expect("connect failed")
            .into_raw_fd();

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_inherit_env();
        std::env::set_var(ENV_FD, raw_fd.to_string());
        std::env::set_var(ENV_NICK, "inheritbot");
        std::env::set_var(ENV_SERVER, &addr);
        std::env::set_var(ENV_CHANNELS, "");
        std::env::set_var(ENV_KA_INTERVAL, "30000");
        std::env::set_var(ENV_KA_TIMEOUT, "10000");

        let state = State::try_inherit_from_env().unwrap().unwrap();
        assert!(state.channels.is_empty());
    }
}
