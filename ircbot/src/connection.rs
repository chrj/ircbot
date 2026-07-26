use std::time::Duration;

use irc_proto::chan::ChannelExt;
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::server::Server;
use crate::transport;
use crate::types::{Channel, Nick};

/// Default interval between client-initiated keepalive pings.
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// Default time to wait for a pong before treating the connection as dead.
pub const DEFAULT_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Default number of messages that may be sent in rapid succession before
/// rate-limiting kicks in (token-bucket burst size).
pub const DEFAULT_FLOOD_BURST: usize = 4;
/// Default minimum interval between messages once the burst budget is exhausted.
pub const DEFAULT_FLOOD_RATE: Duration = Duration::from_millis(500);

/// Default interval between keepnick reclaim attempts when the feature is
/// enabled via [`State::with_keepnick`].
pub const DEFAULT_KEEPNICK_INTERVAL: Duration = Duration::from_secs(60);

/// Everything a caller configures through the `with_*` builders — that is,
/// everything that must outlive the socket it was configured on.
///
/// Kept as one value so [`Blueprint`] can carry it across a reconnect with a
/// single assignment. A new `with_*` setting therefore survives a reconnect by
/// construction: there is no second list of fields to remember to update.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub(crate) keepalive_interval: Duration,
    pub(crate) keepalive_timeout: Duration,
    /// Token-bucket burst: how many messages may be sent immediately before
    /// rate-limiting kicks in.
    pub(crate) flood_burst: usize,
    /// Minimum interval between messages once the burst budget is exhausted.
    pub(crate) flood_rate: Duration,
    /// Custom CTCP `VERSION` reply. When `None`, the framework answers with
    /// `ircbot <crate-version>`; when `Some`, it answers with this string
    /// verbatim. Set via [`State::with_ctcp_version`].
    pub(crate) ctcp_version: Option<String>,
    /// When `Some`, periodically re-attempt to reclaim the originally-requested
    /// nick at this interval whenever the bot is using a different one. `None`
    /// (the default) disables the feature. Set via
    /// [`State::with_keepnick_interval`].
    pub(crate) keepnick_interval: Option<Duration>,
    /// Access-control roles, each mapping a role name to a list of `nick!user@host`
    /// hostmask glob patterns. A command with `role = Some(name)` only fires for
    /// senders matching one of that role's patterns. Set via [`State::with_role`].
    pub(crate) roles: Vec<(String, Vec<String>)>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            keepalive_interval: DEFAULT_KEEPALIVE_INTERVAL,
            keepalive_timeout: DEFAULT_KEEPALIVE_TIMEOUT,
            flood_burst: DEFAULT_FLOOD_BURST,
            flood_rate: DEFAULT_FLOOD_RATE,
            ctcp_version: None,
            keepnick_interval: None,
            roles: Vec::new(),
        }
    }
}

/// Everything needed to establish an equivalent connection: where to connect,
/// as whom, and with which [`Settings`].
///
/// [`crate::internal::run_bot`] takes one before handing the live [`State`] to
/// the read loop, which consumes it. Reconnecting then goes through
/// [`Blueprint::connect`], so the reconnected `State` is configured exactly like
/// the one it replaces.
#[derive(Clone, Debug)]
pub(crate) struct Blueprint {
    nick: Nick,
    server: Server,
    channels: Vec<Channel>,
    settings: Settings,
}

impl Blueprint {
    /// Open a fresh connection configured identically to the one this blueprint
    /// was taken from.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection or handshake fails, exactly as
    /// [`State::connect`] does.
    pub(crate) async fn connect(&self) -> Result<State, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = State::connect(
            self.nick.clone(),
            self.server.clone(),
            self.channels.clone(),
        )
        .await?;
        state.settings = self.settings.clone();
        Ok(state)
    }
}

/// Holds the established connection to an IRC server plus join-on-connect metadata.
pub struct State {
    pub nick: Nick,
    pub channels: Vec<Channel>,
    /// The server this connection was made to, including its transport. Reused
    /// verbatim when reconnecting, so a TLS connection can never come back as
    /// plaintext.
    pub server: Server,
    pub(crate) settings: Settings,
    pub(crate) reader: tokio::io::BufReader<transport::ReadHalf>,
    /// The raw write half; `run_bot_internal` wraps this in a buffered writer and a
    /// dedicated write-loop task.
    pub(crate) write_half: transport::WriteHalf,
    /// The raw file descriptor of the underlying TCP socket, used by the
    /// hot-reload path to pass the live connection to a new binary.
    ///
    /// `None` when the connection cannot be inherited across an `exec`, which
    /// is the case for TLS: the socket survives but the session state needed to
    /// decrypt it does not. [`crate::hot_reload::exec_reload`] then replaces the
    /// binary without handing over the socket, and the successor reconnects.
    #[cfg(unix)]
    pub raw_fd: Option<std::os::unix::io::RawFd>,
}

impl State {
    /// Normalise a channel name: if it doesn't start with a recognised IRC
    /// channel prefix (`#`, `&`, `+`, `!`) a `#` is prepended automatically.
    fn normalise_channel(ch: &str) -> Channel {
        if ch.is_channel_name() {
            Channel::from(ch)
        } else {
            Channel::from(format!("#{ch}"))
        }
    }

    /// Connect to an IRC server, send NICK/USER, and return a `State` ready to run.
    ///
    /// `server` accepts anything that converts into a [`Server`]. A bare
    /// `"host:port"` string connects in plaintext; use `Server::tls` (with the
    /// `tls` feature) for an encrypted connection:
    ///
    /// ```rust,no_run
    /// # use ircbot::{Server, State};
    /// # async fn f() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    /// State::connect("mybot", "irc.example.net:6667", vec![]).await?;
    /// # #[cfg(feature = "tls")]
    /// State::connect("mybot", Server::tls("irc.libera.chat:6697"), vec![]).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Channel names that do not already start with a channel prefix character
    /// (`#`, `&`, `+`, `!`) will automatically be prefixed with `#`, so both
    /// `"general"` and `"#general"` are accepted.
    ///
    /// # Errors
    ///
    /// Returns an error if the TCP connection, the TLS handshake, or the
    /// initial NICK/USER handshake fails.
    pub async fn connect(
        nick: impl Into<Nick>,
        server: impl Into<Server>,
        channels: Vec<Channel>,
    ) -> Result<State, Box<dyn std::error::Error + Send + Sync>> {
        let nick = nick.into();
        let server = server.into();
        let channels: Vec<Channel> = channels
            .iter()
            .map(|c| Self::normalise_channel(c.as_str()))
            .collect();

        let connection = transport::connect(&server).await?;
        #[cfg(unix)]
        let raw_fd = connection.raw_fd;

        let reader = tokio::io::BufReader::new(connection.reader);
        let mut writer = BufWriter::new(connection.writer);

        let nick_line = format!("NICK {nick}\r\n");
        let user_line = format!("USER {nick} 0 * :{nick}\r\n");
        for line in [&nick_line, &user_line] {
            tracing::trace!(
                target: crate::PROTOCOL_LOG_TARGET,
                dir = "send",
                line = %line.trim_end_matches(['\r', '\n']),
            );
        }
        writer.write_all(nick_line.as_bytes()).await?;
        writer.write_all(user_line.as_bytes()).await?;
        writer.flush().await?;

        // Recover the inner write half from the BufWriter.
        let write_half = writer.into_inner();

        Ok(State {
            nick,
            channels,
            server,
            settings: Settings::default(),
            reader,
            write_half,
            #[cfg(unix)]
            raw_fd,
        })
    }

    /// Capture everything needed to re-establish an equivalent connection.
    ///
    /// Taken before the `State` is consumed by the read loop, so the reconnect
    /// that follows can restore the caller's configuration in full.
    pub(crate) fn blueprint(&self) -> Blueprint {
        Blueprint {
            nick: self.nick.clone(),
            server: self.server.clone(),
            channels: self.channels.clone(),
            settings: self.settings.clone(),
        }
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
    /// this is a fresh start, not a reload). A TLS connection is never handed
    /// over, so a reloaded TLS bot always takes this path and reconnects.
    ///
    /// # Errors
    ///
    /// Returns an error if the env vars are malformed or if the fd cannot be
    /// converted to a `TcpStream`.
    #[cfg(unix)]
    pub fn try_inherit_from_env() -> Result<Option<State>, Box<dyn std::error::Error + Send + Sync>>
    {
        use std::os::unix::io::RawFd;

        use crate::hot_reload::{
            ENV_CHANNELS, ENV_FD, ENV_FLOOD_BURST, ENV_FLOOD_RATE, ENV_KA_INTERVAL, ENV_KA_TIMEOUT,
            ENV_NICK, ENV_SERVER,
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
        // Flood-control settings are restored too, falling back to the defaults
        // if absent or malformed — e.g. when the binary that called
        // `exec_reload` predates flood-control serialisation.
        let flood_burst = std::env::var(ENV_FLOOD_BURST)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_FLOOD_BURST);
        let flood_rate = std::env::var(ENV_FLOOD_RATE)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(DEFAULT_FLOOD_RATE, Duration::from_millis);

        // Clear the env vars so they are not accidentally inherited by any
        // child processes the bot might spawn.
        for var in &[
            ENV_FD,
            ENV_NICK,
            ENV_SERVER,
            ENV_CHANNELS,
            ENV_KA_INTERVAL,
            ENV_KA_TIMEOUT,
            ENV_FLOOD_BURST,
            ENV_FLOOD_RATE,
        ] {
            std::env::remove_var(var);
        }

        let channels: Vec<Channel> = if channels_raw.is_empty() {
            vec![]
        } else {
            channels_raw.split(',').map(Channel::from).collect()
        };

        let connection = transport::from_inherited_fd(raw_fd)?;
        let reader = tokio::io::BufReader::new(connection.reader);

        Ok(Some(State {
            nick: Nick::from(nick),
            channels,
            // An inherited connection is always plaintext; TLS sessions cannot
            // survive the `exec` and so are never handed over.
            server: Server::plain(server),
            settings: Settings {
                keepalive_interval: Duration::from_millis(ka_interval_ms),
                keepalive_timeout: Duration::from_millis(ka_timeout_ms),
                flood_burst,
                flood_rate,
                // `ctcp_version`, `keepnick_interval`, and `roles` are re-applied
                // by the bot builder on the re-exec'd process, so they need not
                // be carried through the hot-reload environment.
                ..Settings::default()
            },
            reader,
            write_half: connection.writer,
            raw_fd: connection.raw_fd,
        }))
    }

    /// Override the keepalive ping interval and pong timeout.
    ///
    /// By default the bot sends a `PING` every 30 seconds and waits 10 seconds
    /// for the corresponding `PONG` before treating the connection as dead and
    /// triggering a reconnect.  Call this method (before starting the bot) to
    /// use different values.
    pub fn with_keepalive(mut self, interval: Duration, timeout: Duration) -> Self {
        self.settings.keepalive_interval = interval;
        self.settings.keepalive_timeout = timeout;
        self
    }

    /// Override the flood-control token-bucket settings.
    ///
    /// `burst` is the number of messages that may be sent immediately before
    /// rate-limiting kicks in.  `rate` is the minimum interval between messages
    /// once the burst budget is exhausted.
    pub fn with_flood_control(mut self, burst: usize, rate: Duration) -> Self {
        self.settings.flood_burst = burst;
        self.settings.flood_rate = rate;
        self
    }

    /// Set a custom CTCP `VERSION` reply.
    ///
    /// By default the bot answers a CTCP `VERSION` request with
    /// `ircbot <crate-version>`. Call this (before starting the bot) to reply
    /// with your own identifier instead, e.g. `"mybot 1.2.3"`.
    pub fn with_ctcp_version(mut self, version: impl Into<String>) -> Self {
        self.settings.ctcp_version = Some(version.into());
        self
    }

    /// Enable keepnick: periodically re-attempt to reclaim the
    /// originally-requested nick whenever the bot is using a different one.
    ///
    /// When the requested nick is already taken at connect time the bot falls
    /// back to an alternate (`bot_`, `bot__`, …) and would otherwise keep it
    /// indefinitely. With this enabled, the bot re-sends `NICK <requested>`
    /// every `interval`; once the nick frees up the change succeeds and the bot
    /// returns to its preferred name. While the bot already holds its requested
    /// nick no attempts are made. A retry that fails is harmless — the server
    /// simply replies with `ERR_NICKNAMEINUSE`, which is ignored after
    /// registration.
    ///
    /// This feature is opt-in and disabled by default. Call this (before
    /// starting the bot); see also [`State::with_keepnick`] for the default
    /// interval.
    pub fn with_keepnick_interval(mut self, interval: Duration) -> Self {
        self.settings.keepnick_interval = Some(interval);
        self
    }

    /// Enable keepnick with the default reclaim interval
    /// ([`DEFAULT_KEEPNICK_INTERVAL`], 60 seconds).
    ///
    /// Convenience wrapper around [`State::with_keepnick_interval`].
    pub fn with_keepnick(self) -> Self {
        self.with_keepnick_interval(DEFAULT_KEEPNICK_INTERVAL)
    }

    /// Define an access-control role for command authorization.
    ///
    /// `name` is the role referenced by `#[command(..., role = "name")]`; `masks`
    /// is a set of `nick!user@host` hostmask glob patterns (`*` matches any run
    /// of characters). A command guarded by this role only fires for senders
    /// whose hostmask matches one of the patterns; everyone else is silently
    /// ignored. A command guarded by a role with no configured patterns (or an
    /// unknown role name) therefore never fires — authorization is closed by
    /// default.
    ///
    /// May be called multiple times; patterns accumulate, and the same role name
    /// may be extended across several calls. Call this before starting the bot.
    pub fn with_role(
        mut self,
        name: impl Into<String>,
        masks: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let patterns: Vec<String> = masks.into_iter().map(Into::into).collect();
        self.settings.roles.push((name.into(), patterns));
        self
    }

    /// Returns the configured keepalive interval.
    pub fn keepalive_interval(&self) -> Duration {
        self.settings.keepalive_interval
    }

    /// Returns the configured keepalive timeout.
    pub fn keepalive_timeout(&self) -> Duration {
        self.settings.keepalive_timeout
    }

    /// Returns the configured flood-control burst size.
    pub fn flood_burst(&self) -> usize {
        self.settings.flood_burst
    }

    /// Returns the configured minimum interval between messages once the burst
    /// budget is exhausted.
    pub fn flood_rate(&self) -> Duration {
        self.settings.flood_rate
    }

    /// Returns the configured keepnick reclaim interval, or `None` when the
    /// feature is disabled (the default).
    pub fn keepnick_interval(&self) -> Option<Duration> {
        self.settings.keepnick_interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    // ── normalise_channel ──────────────────────────────────────────────────────

    #[test]
    fn normalise_channel_prefixes_bare_name() {
        assert_eq!(State::normalise_channel("general"), "#general");
    }

    #[test]
    fn normalise_channel_keeps_existing_prefixes() {
        for ch in ["#rust", "&local", "+modeless", "!network"] {
            assert_eq!(State::normalise_channel(ch), ch);
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
        State::connect("tester".to_string(), &addr, vec![Channel::from("general")])
            .await
            .expect("loopback connect failed")
    }

    #[tokio::test]
    async fn connect_normalises_channels() {
        let state = connect_loopback().await;
        assert_eq!(state.channels, vec![Channel::from("#general")]);
    }

    // ── reconnect ──────────────────────────────────────────────────────────────

    /// A loopback listener that keeps accepting, so the same address can be
    /// connected to more than once.
    async fn serve_loopback() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            // Hold every accepted socket so the client side stays connected.
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        addr
    }

    /// Given a connection configured through every `with_*` builder, when it is
    /// re-established from its blueprint, then every setting is still in effect.
    ///
    /// The reconnect path used to copy a hand-maintained subset of the fields,
    /// silently dropping the CTCP version, keepnick, and any future setting.
    #[tokio::test]
    async fn reconnect_preserves_every_configured_setting() {
        let addr = serve_loopback().await;

        let original = State::connect("tester", &addr, vec![Channel::from("general")])
            .await
            .expect("loopback connect failed")
            .with_keepalive(Duration::from_secs(12), Duration::from_secs(4))
            .with_flood_control(9, Duration::from_millis(750))
            .with_ctcp_version("mybot 1.2.3")
            .with_keepnick_interval(Duration::from_secs(15))
            .with_role("admin", ["*!*@trusted.host"]);

        let reconnected = original
            .blueprint()
            .connect()
            .await
            .expect("reconnect failed");

        assert_eq!(reconnected.keepalive_interval(), Duration::from_secs(12));
        assert_eq!(reconnected.keepalive_timeout(), Duration::from_secs(4));
        assert_eq!(reconnected.flood_burst(), 9);
        assert_eq!(reconnected.flood_rate(), Duration::from_millis(750));
        assert_eq!(
            reconnected.keepnick_interval(),
            Some(Duration::from_secs(15))
        );
        assert_eq!(
            reconnected.settings.ctcp_version.as_deref(),
            Some("mybot 1.2.3")
        );
        assert_eq!(
            reconnected.settings.roles,
            vec![("admin".to_string(), vec!["*!*@trusted.host".to_string()])]
        );
    }

    /// The identity of the connection is carried across too, not just its
    /// tunables.
    #[tokio::test]
    async fn reconnect_preserves_nick_server_and_channels() {
        let addr = serve_loopback().await;

        let original = State::connect("tester", &addr, vec![Channel::from("general")])
            .await
            .expect("loopback connect failed");

        let reconnected = original
            .blueprint()
            .connect()
            .await
            .expect("reconnect failed");

        assert_eq!(reconnected.nick, "tester");
        assert_eq!(reconnected.server.addr(), addr);
        assert_eq!(reconnected.channels, vec![Channel::from("#general")]);
    }

    #[tokio::test]
    async fn keepnick_disabled_by_default() {
        let state = connect_loopback().await;
        assert_eq!(state.keepnick_interval(), None);
    }

    #[tokio::test]
    async fn with_keepnick_interval_sets_interval() {
        let state = connect_loopback()
            .await
            .with_keepnick_interval(Duration::from_secs(15));
        assert_eq!(state.keepnick_interval(), Some(Duration::from_secs(15)));
    }

    #[tokio::test]
    async fn with_keepnick_uses_default_interval() {
        let state = connect_loopback().await.with_keepnick();
        assert_eq!(state.keepnick_interval(), Some(DEFAULT_KEEPNICK_INTERVAL));
    }

    // Note: `with_keepalive` and `with_flood_control` are exercised
    // behaviourally elsewhere — keepalive timing in `tests/keepalive.rs` and
    // rate limiting in `tests/flood_control.rs` — so no getter-echo test is
    // needed here.  The keepalive getters are additionally asserted by the
    // `try_inherit_reconstructs_state_from_env` test below.

    // ── try_inherit_from_env (unix) ────────────────────────────────────────────
    //
    // These tests mutate process-global environment variables, so they are
    // serialised behind a shared mutex to avoid racing each other.

    #[cfg(unix)]
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(unix)]
    fn clear_inherit_env() {
        use crate::hot_reload::{
            ENV_CHANNELS, ENV_FD, ENV_FLOOD_BURST, ENV_FLOOD_RATE, ENV_KA_INTERVAL, ENV_KA_TIMEOUT,
            ENV_NICK, ENV_SERVER,
        };
        for var in [
            ENV_FD,
            ENV_NICK,
            ENV_SERVER,
            ENV_CHANNELS,
            ENV_KA_INTERVAL,
            ENV_KA_TIMEOUT,
            ENV_FLOOD_BURST,
            ENV_FLOOD_RATE,
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
        std::env::set_var(crate::hot_reload::ENV_FLOOD_BURST, "9");
        std::env::set_var(crate::hot_reload::ENV_FLOOD_RATE, "750");

        let state = State::try_inherit_from_env()
            .expect("inherit should succeed")
            .expect("env vars present → Some(State)");

        assert_eq!(state.nick, "inheritbot");
        assert_eq!(state.server.addr(), addr);
        // An inherited connection is always plaintext.
        assert!(!state.server.is_tls());
        assert_eq!(
            state.channels,
            vec![Channel::from("#a"), Channel::from("#b")]
        );
        assert_eq!(state.keepalive_interval(), Duration::from_millis(12000));
        assert_eq!(state.keepalive_timeout(), Duration::from_millis(4000));
        // Flood-control settings survive the reload rather than resetting to default.
        assert_eq!(state.flood_burst(), 9);
        assert_eq!(state.flood_rate(), Duration::from_millis(750));

        // try_inherit_from_env clears the env vars once consumed.
        assert!(std::env::var(ENV_FD).is_err());
        assert!(std::env::var(crate::hot_reload::ENV_FLOOD_BURST).is_err());
        assert!(std::env::var(crate::hot_reload::ENV_FLOOD_RATE).is_err());
    }

    /// When the flood-control env vars are absent (e.g. the binary that called
    /// `exec_reload` predates flood serialisation), the defaults are restored.
    #[cfg(unix)]
    #[tokio::test]
    async fn try_inherit_defaults_flood_when_env_absent() {
        use std::os::unix::io::IntoRawFd;

        use crate::hot_reload::{
            ENV_CHANNELS, ENV_FD, ENV_KA_INTERVAL, ENV_KA_TIMEOUT, ENV_NICK, ENV_SERVER,
        };

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
        // Deliberately do NOT set the flood env vars.

        let state = State::try_inherit_from_env().unwrap().unwrap();
        assert_eq!(state.flood_burst(), DEFAULT_FLOOD_BURST);
        assert_eq!(state.flood_rate(), DEFAULT_FLOOD_RATE);
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
