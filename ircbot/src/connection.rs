//! The live connection to an IRC server, and the settings that shape it.
//!
//! [`State`] is what [`State::connect`] returns: an open socket that has
//! finished registering, plus the channels to join. The `with_*` methods on it
//! configure keepalive, flood control, nick recovery, and roles before the bot
//! starts.
//!
//! Registration is the `NICK`/`USER` handshake, and — when the [`Server`]
//! carries credentials — the `PASS` line, the IRCv3 capability exchange, and
//! SASL, all of which run before `CAP END` lets the server finish. Those
//! credentials live on the `Server` rather than behind a `with_*` method
//! because they are needed here, before any such method can be called.
//!
//! Each `DEFAULT_*` constant in this module gives the value the matching
//! setting starts at. Nick recovery is the exception: it stays off until you
//! call [`State::with_keepnick`] or [`State::with_keepnick_interval`].

use std::time::Duration;

use irc_proto::chan::ChannelExt;
use irc_proto::CapSubCommand;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufWriter};

use crate::auth::Auth;
use crate::context::sanitize;
use crate::irc::{Command, Message, Response};
use crate::server::Server;
use crate::transport;
use crate::types::{Channel, Nick};
use crate::BoxError;

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

/// How long the IRCv3 capability and SASL exchange may take before the
/// connection is given up on.
///
/// Generous enough for a services daemon that is slow to answer, while still
/// bounding a server that stops replying part-way through: without a bound the
/// bot would wait for `CAP ACK` forever, never reaching the read loop and never
/// reconnecting.
pub const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Longest `AUTHENTICATE` payload the protocol allows in one line; longer
/// responses are split across several (IRCv3 SASL specification).
const SASL_CHUNK_LEN: usize = 400;

/// Most lines the capability exchange will read before giving up.
///
/// A real exchange takes under a dozen. The bound is what stops a server that
/// streams lines instead of answering from growing the buffered-line list
/// without limit — the timeout alone does not, since it bounds time rather than
/// memory.
const MAX_HANDSHAKE_LINES: usize = 1024;

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

// ─── registration handshake ──────────────────────────────────────────────────

/// Write one line to the server, appending the IRC line terminator and logging
/// it on the protocol target.
async fn send(
    writer: &mut BufWriter<transport::WriteHalf>,
    line: &str,
) -> Result<(), std::io::Error> {
    send_secret(writer, line, line).await
}

/// Write one line, logging `shown` in place of the line itself.
///
/// `PASS` and `AUTHENTICATE` carry credentials, and the protocol log is a
/// `trace!` an operator may well have switched on — so what goes on the wire
/// and what goes in the log deliberately differ for those two.
async fn send_secret(
    writer: &mut BufWriter<transport::WriteHalf>,
    line: &str,
    shown: &str,
) -> Result<(), std::io::Error> {
    tracing::trace!(target: crate::PROTOCOL_LOG_TARGET, dir = "send", line = %shown);
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\r\n").await?;
    writer.flush().await
}

/// Send a SASL response, split into the chunks the protocol allows.
///
/// An empty response, and one whose length is an exact multiple of
/// [`SASL_CHUNK_LEN`], is followed by a lone `+` so the server can tell the
/// payload has ended rather than waiting for a continuation that never comes.
async fn send_sasl_response(
    writer: &mut BufWriter<transport::WriteHalf>,
    response: &str,
) -> Result<(), std::io::Error> {
    // base64 output is pure ASCII, so chunking by byte offset can never land
    // mid-character.
    for chunk in response.as_bytes().chunks(SASL_CHUNK_LEN) {
        let chunk = String::from_utf8_lossy(chunk);
        send_secret(
            writer,
            &format!("AUTHENTICATE {chunk}"),
            "AUTHENTICATE <redacted>",
        )
        .await?;
    }
    if response.len().is_multiple_of(SASL_CHUNK_LEN) {
        send(writer, "AUTHENTICATE +").await?;
    }
    Ok(())
}

/// Split the payload of a `CAP … LS`/`ACK`/`NAK` line into "is this batch
/// continued?" and the capability list itself.
///
/// `irc-proto` puts the list in the last populated parameter: a final
/// `CAP * LS :a b` parses with the list in `arg` and nothing trailing, while a
/// continued `CAP * LS * :a b` parses with the `*` marker in `arg` and the list
/// trailing.
fn cap_payload<'a>(arg: Option<&'a String>, trailing: Option<&'a String>) -> (bool, &'a str) {
    match (arg, trailing) {
        (Some(marker), Some(caps)) => (marker == "*", caps.as_str()),
        (Some(caps), None) => (false, caps.as_str()),
        (None, caps) => (false, caps.map_or("", String::as_str)),
    }
}

/// Look up `name` among the capabilities the server advertised, returning its
/// value (the part after `=`, empty when it has none).
///
/// Capability names are case-sensitive per the IRCv3 specification.
fn advertised<'a>(available: &'a [String], name: &str) -> Option<&'a str> {
    available.iter().find_map(|cap| {
        let (cap_name, value) = cap.split_once('=').unwrap_or((cap.as_str(), ""));
        (cap_name == name).then_some(value)
    })
}

/// Run the IRCv3 capability exchange, and the SASL exchange inside it, until
/// the server is ready to complete registration.
///
/// Returns the lines that arrived during the exchange but did not belong to it
/// — `ERR_NICKNAMEINUSE` above all, which the server sends as soon as it reads
/// our `NICK`. They are handed back in arrival order so the read loop can
/// process them as if they had come in normally.
///
/// # Errors
///
/// Returns an error if the connection drops or stalls mid-exchange, or if SASL
/// was configured and could not be completed — an unauthenticated connection is
/// not silently accepted in its place.
async fn negotiate(
    reader: &mut tokio::io::BufReader<transport::ReadHalf>,
    writer: &mut BufWriter<transport::WriteHalf>,
    auth: &Auth,
) -> Result<Vec<String>, BoxError> {
    let mut pending: Vec<String> = Vec::new();
    let wanted = auth.wanted_caps();
    if wanted.is_empty() {
        return Ok(pending);
    }

    // Capabilities advertised so far, each still in `name` or `name=value`
    // form. `CAP LS` may be split across several lines, so they accumulate
    // until the batch ends.
    let mut available: Vec<String> = Vec::new();
    // Whether an `AUTHENTICATE <mechanism>` has gone out. A success numeric that
    // arrives before one did not answer anything we sent, so it does not count.
    let mut sasl_started = false;
    let deadline = tokio::time::Instant::now() + REGISTRATION_TIMEOUT;

    for _ in 0..MAX_HANDSHAKE_LINES {
        let mut line = String::new();
        let read = tokio::time::timeout_at(deadline, reader.read_line(&mut line))
            .await
            .map_err(|_| -> BoxError {
                format!(
                    "the server stopped responding during IRCv3 capability negotiation \
                     (waited {REGISTRATION_TIMEOUT:?}). Make sure that the port speaks IRC and \
                     that the network supports CAP. A server without CAP support answers with an \
                     error, not with silence"
                )
                .into()
            })??;
        if read == 0 {
            return Err("the server closed the connection during IRCv3 capability \
                        negotiation. A network does this when the server password passed to \
                        Server::with_password is wrong"
                .into());
        }

        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        tracing::trace!(target: crate::PROTOCOL_LOG_TARGET, dir = "recv", %line);

        let Ok(msg) = line.parse::<Message>() else {
            pending.push(line.to_string());
            continue;
        };

        match &msg.command {
            // A server may ping mid-handshake; an unanswered one gets us
            // disconnected before registration ever completes.
            Command::PING(server, _) => send(writer, &format!("PONG :{server}")).await?,

            Command::CAP(_, CapSubCommand::LS, arg, trailing) => {
                let (more, caps) = cap_payload(arg.as_ref(), trailing.as_ref());
                available.extend(caps.split_whitespace().map(str::to_string));
                if more {
                    continue;
                }

                let requested: Vec<&str> = wanted
                    .iter()
                    .copied()
                    .filter(|cap| advertised(&available, cap).is_some())
                    .collect();

                if let Some(sasl) = &auth.sasl {
                    let Some(mechanisms) = advertised(&available, "sasl") else {
                        return Err(format!(
                            "SASL authentication was configured but {} does not offer the sasl \
                             capability. Drop the with_sasl_* call, or connect to a server that \
                             supports SASL",
                            server_name(&msg)
                        )
                        .into());
                    };
                    // With `CAP LS 302` the value lists the mechanisms; older
                    // servers advertise a bare `sasl` and accept any of them,
                    // so an empty value is not a rejection.
                    if !mechanisms.is_empty()
                        && !mechanisms
                            .split(',')
                            .any(|m| m.eq_ignore_ascii_case(sasl.mechanism()))
                    {
                        return Err(format!(
                            "the server does not support SASL {}. It offers {mechanisms}. Pick \
                             a mechanism from that list",
                            sasl.mechanism()
                        )
                        .into());
                    }
                }

                for cap in wanted.iter().filter(|c| !requested.contains(c)) {
                    tracing::debug!(capability = cap, "capability not advertised — skipping");
                }

                if requested.is_empty() {
                    send(writer, "CAP END").await?;
                    return Ok(pending);
                }
                send(writer, &format!("CAP REQ :{}", requested.join(" "))).await?;
            }

            Command::CAP(_, CapSubCommand::ACK, arg, trailing) => {
                let (_, caps) = cap_payload(arg.as_ref(), trailing.as_ref());
                tracing::debug!(capabilities = caps, "capabilities acknowledged");

                let acked_sasl = caps.split_whitespace().any(|c| c == "sasl");
                match (&auth.sasl, acked_sasl) {
                    (Some(sasl), true) => {
                        send(writer, &format!("AUTHENTICATE {}", sasl.mechanism())).await?;
                        sasl_started = true;
                    }
                    (Some(sasl), false) => {
                        return Err(format!(
                            "the server acknowledged {caps} but not sasl, so the bot cannot \
                             authenticate with SASL {}. Services are usually down when this \
                             happens. Retry, or drop the with_sasl_* call to connect \
                             unauthenticated",
                            sasl.mechanism()
                        )
                        .into());
                    }
                    (None, _) => {
                        send(writer, "CAP END").await?;
                        return Ok(pending);
                    }
                }
            }

            Command::CAP(_, CapSubCommand::NAK, arg, trailing) => {
                let (_, caps) = cap_payload(arg.as_ref(), trailing.as_ref());
                if auth.sasl.is_some() && caps.split_whitespace().any(|c| c == "sasl") {
                    return Err(format!(
                        "the server refused the sasl capability ({caps}), so the bot cannot \
                         authenticate. Services are usually down when this happens. Retry, or \
                         drop the with_sasl_* call to connect unauthenticated"
                    )
                    .into());
                }
                tracing::warn!(capabilities = caps, "capabilities refused by the server");
                send(writer, "CAP END").await?;
                return Ok(pending);
            }

            // The server is ready for the mechanism's response. `+` means it
            // sent no challenge of its own, which is all PLAIN and EXTERNAL
            // ever see.
            Command::AUTHENTICATE(_) => {
                let Some(sasl) = &auth.sasl else {
                    pending.push(line.to_string());
                    continue;
                };
                send_sasl_response(writer, &sasl.response()).await?;
            }

            Command::Response(Response::RPL_LOGGEDIN, args) => {
                // "<nick> <mask> <account> :You are now logged in as <account>"
                if let Some(account) = args.get(2) {
                    tracing::info!(%account, "authenticated with SASL");
                }
            }

            Command::Response(Response::RPL_SASLSUCCESS, _) if sasl_started => {
                send(writer, "CAP END").await?;
                return Ok(pending);
            }

            Command::Response(
                response @ (Response::ERR_NICKLOCKED
                | Response::ERR_SASLFAIL
                | Response::ERR_SASLTOOLONG
                | Response::ERR_SASLABORT
                | Response::ERR_SASLALREADY),
                args,
            ) => {
                let detail = args.last().map_or("no detail given", String::as_str);
                return Err(format!(
                    "SASL authentication failed: {detail} ({response:?}). Correct the account \
                     name and password passed to with_sasl_plain, or the client certificate \
                     registered with the network for with_sasl_external"
                )
                .into());
            }

            // A server old enough to have no CAP command answers this way.
            Command::Response(Response::ERR_UNKNOWNCOMMAND, args)
                if args.iter().any(|a| a.eq_ignore_ascii_case("CAP")) =>
            {
                if auth.sasl.is_some() {
                    return Err(
                        "SASL authentication was configured but the server does not implement \
                         CAP, so it cannot support SASL. Drop the with_sasl_* call, or connect to \
                         a server that supports IRCv3"
                            .into(),
                    );
                }
                tracing::warn!("server does not support CAP — continuing without capabilities");
                return Ok(pending);
            }

            // Everything else belongs to the read loop, not to this exchange.
            _ => pending.push(line.to_string()),
        }
    }

    Err(format!(
        "the server sent more than {MAX_HANDSHAKE_LINES} lines without finishing IRCv3 \
         capability negotiation. Make sure that the address points at an IRC server and not at \
         another protocol"
    )
    .into())
}

/// The server's own name, taken from a message's prefix, for use in an error.
fn server_name(msg: &Message) -> &str {
    match msg.prefix.as_ref() {
        Some(irc_proto::Prefix::ServerName(name)) => name.as_str(),
        _ => "the server",
    }
}

/// Holds the established connection to an IRC server plus join-on-connect metadata.
pub struct State {
    /// The nick registered with the server during the handshake.
    pub nick: Nick,
    /// The channels joined after connect, and rejoined after a reconnect.
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
    /// Lines that arrived during the capability exchange but were not part of
    /// it. The read loop drains these before reading the socket, so a message
    /// the server sent early — `ERR_NICKNAMEINUSE`, typically — is dispatched
    /// in arrival order rather than lost.
    pub(crate) pending_lines: Vec<String>,
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
    /// When the `server` carries credentials, this also runs the IRCv3
    /// capability exchange and SASL before returning, so the connection is
    /// already authenticated by the time the bot joins anything.
    ///
    /// # Errors
    ///
    /// Returns an error if the TCP connection, the TLS handshake, or the
    /// registration handshake fails. A configured SASL exchange that the server
    /// refuses counts as a failure: an unauthenticated connection is never
    /// silently accepted in its place.
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

        let mut reader = tokio::io::BufReader::new(connection.reader);
        let mut writer = BufWriter::new(connection.writer);

        // `CAP LS` goes first: a server that sees it holds registration open
        // until `CAP END`, which is the window SASL has to complete in. Sent
        // after `NICK`/`USER` it would race the server's own completion of
        // registration, and SASL is refused once registration is done.
        if server.auth.negotiates_caps() {
            send(&mut writer, "CAP LS 302").await?;
        }
        if let Some(password) = &server.auth.password {
            send_secret(
                &mut writer,
                &format!("PASS :{}", sanitize(password)),
                "PASS :<redacted>",
            )
            .await?;
        }
        send(&mut writer, &format!("NICK {nick}")).await?;
        send(&mut writer, &format!("USER {nick} 0 * :{nick}")).await?;

        let pending_lines = negotiate(&mut reader, &mut writer, &server.auth).await?;

        // Recover the inner write half from the BufWriter.
        let write_half = writer.into_inner();

        Ok(State {
            nick,
            channels,
            server,
            settings: Settings::default(),
            reader,
            write_half,
            pending_lines,
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
            // An inherited connection is already registered, so no capability
            // exchange runs and nothing can have been read ahead of the loop.
            pending_lines: Vec::new(),
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

    // ── CAP line payloads ──────────────────────────────────────────────────────
    //
    // `cap_payload` exists because `irc-proto` places the capability list in a
    // different parameter depending on whether the batch is continued. These
    // tests parse real wire lines rather than constructing `Command::CAP` by
    // hand, so they would catch that placement changing.

    fn parse_cap(line: &str) -> (bool, String) {
        let msg: Message = line.parse().expect("a valid CAP line");
        let Command::CAP(_, _, arg, trailing) = &msg.command else {
            panic!("not a CAP command: {line}");
        };
        let (more, caps) = cap_payload(arg.as_ref(), trailing.as_ref());
        (more, caps.to_string())
    }

    #[test]
    fn a_final_cap_ls_yields_its_capability_list() {
        let (more, caps) = parse_cap(":srv CAP * LS :sasl=PLAIN multi-prefix");
        assert!(!more);
        assert_eq!(caps, "sasl=PLAIN multi-prefix");
    }

    #[test]
    fn a_continued_cap_ls_is_flagged_and_still_yields_its_list() {
        let (more, caps) = parse_cap(":srv CAP * LS * :sasl=PLAIN multi-prefix");
        assert!(more);
        assert_eq!(caps, "sasl=PLAIN multi-prefix");
    }

    #[test]
    fn a_cap_ack_yields_its_capability_list() {
        let (more, caps) = parse_cap(":srv CAP * ACK :sasl");
        assert!(!more);
        assert_eq!(caps, "sasl");
    }

    // ── advertised capabilities ────────────────────────────────────────────────

    #[test]
    fn advertised_returns_the_value_after_the_equals_sign() {
        let caps = vec!["sasl=PLAIN,EXTERNAL".to_string(), "server-time".to_string()];
        assert_eq!(advertised(&caps, "sasl"), Some("PLAIN,EXTERNAL"));
    }

    /// A pre-302 server advertises a bare `sasl` with no mechanism list. That
    /// is "supported, mechanisms unknown", not "unsupported".
    #[test]
    fn advertised_returns_an_empty_value_for_a_valueless_capability() {
        let caps = vec!["sasl".to_string()];
        assert_eq!(advertised(&caps, "sasl"), Some(""));
    }

    #[test]
    fn advertised_returns_none_when_absent() {
        let caps = vec!["server-time".to_string()];
        assert_eq!(advertised(&caps, "sasl"), None);
    }

    /// A capability whose name merely starts with the one we want must not
    /// count as a match.
    #[test]
    fn advertised_does_not_match_a_name_prefix() {
        let caps = vec!["sasl-not-really".to_string()];
        assert_eq!(advertised(&caps, "sasl"), None);
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
