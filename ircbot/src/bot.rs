//! The dispatch loop and the trigger matching behind it.
//!
//! [`run_bot_internal`] owns a connected [`State`] and runs until the
//! connection ends. It reads a line, parses it, and tests it against every
//! [`HandlerEntry`] in the current [`HandlerSet`], then spawns a task per match.
//!
//! Most users reach this module through the `#[bot]` macro rather than
//! directly. The matching functions are public so that handlers and tests can
//! ask the same questions the loop asks: [`check_trigger`] reports whether a
//! message fires a trigger, [`glob_match`] backs the `*` patterns, and
//! [`authorized`] tests a hostmask against a role.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use leaky_bucket::RateLimiter;
use regex::Regex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufWriter};
use tokio::sync::mpsc;

use crate::{
    connection::{Settings, State},
    context::{make_messages, nick_eq, sanitize, Context, User},
    handler::{HandlerEntry, Trigger},
    irc::{CtcpMessage, Message},
    logging::PROTOCOL_LOG_TARGET,
    types::{Nick, Target},
    BoxError,
};
use irc_proto::{prefix::Prefix, Command, Response};

/// Command prefix recognised by the bot (e.g. `!ping`).
const CMD_PREFIX: char = '!';

/// The token sent in our client-initiated keepalive `PING`.
const KEEPALIVE_TOKEN: &str = "ircbot-keepalive";

/// How many alternate nicks to try when the requested nick is already in use
/// before giving up on registration.  Each `ERR_NICKNAMEINUSE` (433) /
/// `ERR_UNAVAILRESOURCE` (437) reply triggers one further attempt.
const MAX_NICK_ATTEMPTS: u32 = 8;

/// Upper bound on how long the cron supervisor sleeps in one cycle.  When the
/// next scheduled fire is further away than this (or there are no cron handlers
/// at all), the supervisor still wakes to re-read the live handler set, so a
/// cron handler added by a hot-reload is picked up within this window.
const CRON_RESCAN_INTERVAL: Duration = Duration::from_secs(60);

/// A shareable, atomically-swappable set of handler entries.
///
/// The outer [`Arc`] allows the handle to be cloned cheaply.  The [`RwLock`]
/// serialises writes.  The inner [`Arc`] lets a reader snapshot the current
/// handler list with a single cheap `Arc::clone` — no lock is held across
/// `.await` points.
pub type HandlerSet<T> = Arc<RwLock<Arc<Vec<HandlerEntry<T>>>>>;

// ─── public entry-point ──────────────────────────────────────────────────────

/// Handles IRC messages, dispatching to registered handlers.
///
/// Sends a periodic `PING` to the server and breaks out of the read loop (so
/// the caller can reconnect) if the corresponding `PONG` is not received within
/// the configured timeout.
///
/// The `handlers` are read from a shared [`HandlerSet`] on every incoming
/// message, so they can be swapped atomically at any point without
/// disconnecting from IRC.
///
/// # Errors
///
/// Returns an error if reading from the connection fails.
pub async fn run_bot_internal<T: Send + Sync + 'static>(
    bot: Arc<T>,
    state: State,
    handlers: HandlerSet<T>,
) -> Result<(), BoxError> {
    let State {
        nick,
        channels,
        server: _,
        settings:
            Settings {
                keepalive_interval,
                keepalive_timeout,
                flood_burst,
                flood_rate,
                ctcp_version,
                keepnick_interval,
                roles,
            },
        reader,
        write_half,
        pending_lines,
        #[cfg(unix)]
            raw_fd: _,
    } = state;

    // Create the mpsc write channel.
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();

    // Spawn the write loop — drains the channel into the TCP write half,
    // enforcing a token-bucket flood-control policy so that the bot cannot
    // send messages faster than the server allows.
    let write_task = tokio::spawn(async move {
        let mut writer = BufWriter::new(write_half);

        // Token-bucket flood control: start with a full burst budget and refill
        // one token every `flood_rate`. `acquire_one` returns immediately while
        // the budget lasts, then waits for the next refill once it is exhausted,
        // so the bot never sends faster than the server allows.  `max` is
        // clamped to at least 1 (a 0 burst rate-limits from the very first
        // message, which is honoured by the empty `initial` budget below).
        let limiter = RateLimiter::builder()
            .max(flood_burst.max(1))
            .initial(flood_burst)
            .refill(1)
            .interval(flood_rate)
            .build();

        while let Some(msg) = write_rx.recv().await {
            limiter.acquire_one().await;

            tracing::trace!(
                target: PROTOCOL_LOG_TARGET,
                dir = "send",
                line = %msg.trim_end_matches(['\r', '\n']),
            );

            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    });

    // Spawn a single supervisor task that fires Cron-triggered handlers on
    // schedule.  Unlike a per-handler snapshot, it re-reads the live handler set
    // on every cycle, so cron handlers added, removed, or replaced via
    // [`crate::ReloadHandle::reload`] take effect without waiting for a
    // reconnect.  The task is aborted when this connection is torn down and
    // re-spawned on reconnect.
    //
    // `nick` is the originally-requested nick and stays fixed as the base for
    // generating fallbacks; `bot_nick` tracks the nick we are actually using and
    // is updated if the server reports the requested one is taken.
    let mut bot_nick = nick.clone();
    let cron_task = tokio::spawn(run_cron_supervisor(
        Arc::clone(&bot),
        Arc::clone(&handlers),
        write_tx.clone(),
        bot_nick.clone(),
    ));

    // Shared view of the nick we are actually using, kept in sync with
    // `bot_nick` at every point it changes (registration fallback and our own
    // post-registration NICK changes).  `registered` flips to `true` on
    // RPL_WELCOME.  Both are read by the optional keepnick task below.
    let current_nick = Arc::new(RwLock::new(bot_nick.clone()));
    let registered = Arc::new(AtomicBool::new(false));

    // Keepnick: when enabled, periodically re-attempt to reclaim the
    // originally-requested nick while we are using a different one.  A failed
    // attempt just yields an ERR_NICKNAMEINUSE, which is ignored after
    // registration; once the nick frees up the change succeeds and this becomes
    // a no-op until the nick is lost again.
    let keepnick_task = keepnick_interval.map(|interval| {
        let desired = nick.clone();
        let stealer_write_tx = write_tx.clone();
        let current_nick = Arc::clone(&current_nick);
        let registered = Arc::clone(&registered);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                if !registered.load(Ordering::Relaxed) {
                    continue;
                }
                let current = current_nick
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if current != desired {
                    tracing::debug!(%desired, %current, "keepnick: reclaiming");
                    if stealer_write_tx
                        .send(format!("NICK {desired}\r\n"))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        })
    });

    // Keepalive: set to `true` on startup (no ping pending) and whenever we
    // receive a matching PONG.  The keepalive task resets it to `false` before
    // each PING, then checks it again after the timeout.
    let pong_received = Arc::new(AtomicBool::new(true));
    let pong_received_keepalive = Arc::clone(&pong_received);
    let keepalive_write_tx = write_tx.clone();
    let (keepalive_fail_tx, keepalive_fail_rx) = tokio::sync::oneshot::channel::<()>();

    let keepalive_task = tokio::spawn(async move {
        let mut fail_tx = Some(keepalive_fail_tx);
        loop {
            tokio::time::sleep(keepalive_interval).await;
            pong_received_keepalive.store(false, Ordering::Relaxed);
            if keepalive_write_tx
                .send(format!("PING {KEEPALIVE_TOKEN}\r\n"))
                .is_err()
            {
                break;
            }
            tokio::time::sleep(keepalive_timeout).await;
            if !pong_received_keepalive.load(Ordering::Relaxed) {
                tracing::warn!("keepalive timeout — reconnecting");
                if let Some(tx) = fail_tx.take() {
                    let _ = tx.send(());
                }
                break;
            }
        }
    });

    let mut joined = false;
    // A nick change of the bot itself, applied after the line that carried it
    // is dispatched.
    let mut pending_nick: Option<Nick> = None;
    // Number of alternate-nick attempts made so far (after the initial NICK).
    let mut nick_attempt = 0u32;
    let mut lines = reader.lines();
    // Lines the capability exchange read ahead of this loop; drained before the
    // socket so ordering is preserved.
    let mut pending_lines = pending_lines.into_iter();
    let mut keepalive_fail_rx = keepalive_fail_rx;

    // Run the read loop; collect any IO error so we can clean up first.
    let loop_result: Result<(), BoxError> = async {
        loop {
            tokio::select! {
                result = next_line(&mut pending_lines, &mut lines) => {
                    let Some(line) = result? else { break; };
                    let line = line.trim_end_matches('\r').to_string();
                    if line.is_empty() {
                        continue;
                    }

                    tracing::trace!(target: PROTOCOL_LOG_TARGET, dir = "recv", %line);

                    let Ok(msg) = line.parse::<Message>() else {
                        continue;
                    };
                    match &msg.command {
                        Command::PING(srv, _) => {
                            if let Err(e) = write_tx.send(format!("PONG :{srv}\r\n")) {
                                tracing::error!(error = %e, "failed to send PONG");
                            }
                        }
                        Command::PONG(a, b) => {
                            // The keepalive token is echoed back in the
                            // trailing position: "PONG server :token" → b,
                            // or without a server: "PONG :token" → a.
                            let token = b.as_deref().unwrap_or(a.as_str());
                            if token == KEEPALIVE_TOKEN {
                                pong_received.store(true, Ordering::Relaxed);
                            }
                        }
                        Command::Response(Response::RPL_WELCOME, _) => {
                            registered.store(true, Ordering::Relaxed);
                            if !joined {
                                joined = true;
                                for ch in &channels {
                                    if let Err(e) = write_tx.send(format!("JOIN {ch}\r\n")) {
                                        tracing::error!(channel = %ch, error = %e, "failed to send JOIN");
                                    }
                                }
                            }
                        }
                        Command::Response(
                            Response::ERR_NICKNAMEINUSE | Response::ERR_UNAVAILRESOURCE,
                            _,
                        ) => {
                            // Only renegotiate before registration completes.
                            // A 433/437 after we've joined refers to a later
                            // NICK-change attempt and is left for handlers.
                            if !joined {
                                nick_attempt += 1;
                                if nick_attempt <= MAX_NICK_ATTEMPTS {
                                    let candidate = fallback_nick(nick.as_str(), nick_attempt);
                                    tracing::warn!(
                                        current = %bot_nick,
                                        %candidate,
                                        "nick unavailable — retrying"
                                    );
                                    if let Err(e) =
                                        write_tx.send(format!("NICK {candidate}\r\n"))
                                    {
                                        tracing::error!(%candidate, error = %e, "failed to send NICK");
                                    }
                                    bot_nick = Nick::from(candidate);
                                    *current_nick
                                        .write()
                                        .unwrap_or_else(|e| e.into_inner()) =
                                        bot_nick.clone();
                                } else {
                                    tracing::error!(
                                        attempts = MAX_NICK_ATTEMPTS,
                                        "giving up on registration"
                                    );
                                }
                            }
                        }
                        Command::NICK(new_nick) => {
                            // Keep `bot_nick` in sync when the change is our
                            // own, so the keepnick knows once it has
                            // succeeded (and stops retrying). The new nick is
                            // applied after the dispatch of this line, so that
                            // the line still counts as one from the bot itself
                            // and handlers do not see their own rename.
                            if let Some(Prefix::Nickname(old, ..)) = msg.prefix.as_ref() {
                                if old.as_str() == bot_nick.as_str() {
                                    pending_nick = Some(Nick::from(new_nick.clone()));
                                }
                            }
                        }
                        _ => {}
                    }

                    let errors = handle_message(
                        &bot,
                        &handlers,
                        &msg,
                        &bot_nick,
                        ctcp_version.as_deref(),
                        &roles,
                        write_tx.clone(),
                    )
                    .await;
                    for error in errors {
                        tracing::error!(%error, "handler error");
                    }

                    if let Some(new_nick) = pending_nick.take() {
                        bot_nick = new_nick;
                        *current_nick.write().unwrap_or_else(|e| e.into_inner()) =
                            bot_nick.clone();
                    }
                }
                _ = &mut keepalive_fail_rx => {
                    // Keepalive timed out — exit so the caller can reconnect.
                    break;
                }
            }
        }
        Ok(())
    }
    .await;

    // Always clean up the keepalive, cron, keepnick, and write tasks before
    // returning.
    keepalive_task.abort();
    cron_task.abort();
    if let Some(task) = keepnick_task {
        task.abort();
    }
    drop(write_tx);
    let _ = write_task.await;

    loop_result
}

/// Yield the next line to dispatch: one the registration handshake read ahead
/// of the loop if any are left, otherwise the next one off the socket.
///
/// Cancel-safe, as the `select!` in the read loop requires: the `pending`
/// branch returns without ever awaiting, so it cannot be dropped part-way and
/// lose a line, and `next_line` is cancel-safe in its own right.
async fn next_line(
    pending: &mut std::vec::IntoIter<String>,
    lines: &mut tokio::io::Lines<tokio::io::BufReader<crate::transport::ReadHalf>>,
) -> std::io::Result<Option<String>> {
    match pending.next() {
        Some(line) => Ok(Some(line)),
        None => lines.next_line().await,
    }
}

// ─── cron supervisor ─────────────────────────────────────────────────────────

/// Drive every [`Trigger::Cron`] handler in `handlers`.
///
/// Each cycle re-reads the live handler set, so cron handlers added, removed,
/// or replaced via [`crate::ReloadHandle::reload`] take effect immediately,
/// then sleeps until the earliest upcoming occurrence (capped at
/// [`CRON_RESCAN_INTERVAL`]) and fires every handler that is due.  Due handlers
/// run sequentially, mirroring the message-dispatch path.
async fn run_cron_supervisor<T: Send + Sync + 'static>(
    bot: Arc<T>,
    handlers: HandlerSet<T>,
    tx: mpsc::UnboundedSender<String>,
    bot_nick: Nick,
) {
    loop {
        // Reference instant for this cycle.  All "next occurrence" computations
        // are relative to it, so the pre-sleep and post-sleep views agree.
        let now = chrono::Utc::now();

        // Earliest upcoming fire across all cron handlers in the live set.
        let fire_at = {
            let live = snapshot(&handlers);
            live.iter()
                .filter_map(|e| next_cron_fire(&e.trigger, &now))
                .min()
        };

        // Sleep until that fire, capped so reloads are noticed within a bounded
        // window (and so we idle gracefully when there are no cron handlers).
        let wait = fire_at.map_or(CRON_RESCAN_INTERVAL, |at| {
            (at - now)
                .to_std()
                .unwrap_or(Duration::ZERO)
                .min(CRON_RESCAN_INTERVAL)
        });
        tokio::time::sleep(wait).await;

        let Some(fire_at) = fire_at else { continue };
        if chrono::Utc::now() < fire_at {
            // Woken by the rescan cap before the fire was due — re-evaluate.
            continue;
        }

        // Re-read the live set so reloaded handler bodies fire, then run every
        // cron handler whose next occurrence after `now` has now arrived.
        let live = snapshot(&handlers);
        let now2 = chrono::Utc::now();
        for entry in live.iter() {
            let Trigger::Cron { target, .. } = &entry.trigger else {
                continue;
            };
            let Some(next) = next_cron_fire(&entry.trigger, &now) else {
                continue;
            };
            if next > now2 {
                continue; // not due yet
            }
            let cron_target = target.clone().unwrap_or_default();
            let ctx = Context {
                tx: tx.clone(),
                target: Target::from_raw(&cron_target),
                sender: None,
                raw: synthesize_cron_message(bot_nick.as_str()),
                bot_nick: bot_nick.clone(),
                captures: vec![],
            };
            if let Err(e) = (entry.handler)(Arc::clone(&bot), ctx).await {
                tracing::error!(error = %e, "cron handler error");
            }
        }
    }
}

/// Snapshot the live handler list with a single cheap `Arc::clone`, holding the
/// read lock only momentarily.
fn snapshot<T>(handlers: &HandlerSet<T>) -> Arc<Vec<HandlerEntry<T>>> {
    let guard = handlers.read().unwrap_or_else(|e| e.into_inner());
    Arc::clone(&*guard)
}

/// The next fire time (in UTC) strictly after `after` for a [`Trigger::Cron`],
/// or `None` for any other trigger or an unparseable schedule/timezone.
///
/// The `#[on(cron = …)]` macro validates the expression and timezone at compile
/// time; only the lower-level manual API can produce an invalid one, which
/// simply never fires.
fn next_cron_fire(
    trigger: &Trigger,
    after: &chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let Trigger::Cron { schedule, tz, .. } = trigger else {
        return None;
    };
    let schedule: cron::Schedule = schedule.parse().ok()?;
    let tz: chrono_tz::Tz = tz.parse().ok()?;
    schedule
        .after(&after.with_timezone(&tz))
        .next()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Build the synthetic [`Message`] used as the `raw` field of a cron-fired
/// [`Context`].  Cron handlers have no originating IRC message, so a benign one
/// tagged with a `cron` pseudo-source is fabricated.
fn synthesize_cron_message(bot_nick: &str) -> Message {
    format!(":{bot_nick}!cron@cron PING :cron")
        .parse::<Message>()
        .unwrap_or_else(|_| {
            format!(":{bot_nick}!cron@cron PRIVMSG #cron :cron")
                .parse()
                .unwrap()
        })
}

// ─── trigger matching ────────────────────────────────────────────────────────

/// Returns `Some(captures)` if `msg` matches `trigger`, `None` otherwise.
///
/// The text is matched without its IRC formatting codes, so a pattern does not
/// need to know about bold or colour. The captures carry the text without the
/// codes too. [`Context::message_text`](crate::Context::message_text) still
/// gives the text with them.
///
/// A `PRIVMSG` that carries a CTCP message (for example a `/me` action) is not
/// chat text. Only [`Trigger::Action`] and [`Trigger::Ctcp`] can match it.
#[must_use]
pub fn check_trigger(trigger: &Trigger, msg: &Message, bot_nick: &str) -> Option<Vec<String>> {
    let text = crate::format::strip_cow(trailing_param(msg).unwrap_or(""));
    check_trigger_text(trigger, msg, bot_nick, &text)
}

/// The body of [`check_trigger`], with the text to match given by the caller.
///
/// The dispatch strips the text of a message once and gives it to every
/// trigger, rather than once per trigger. A handler entry with `raw_text` gets
/// the text with the codes here.
///
/// The capture of a [`Trigger::Ctcp`] is an exception: it always carries the
/// payload as it arrived, because a CTCP payload is protocol data.
fn check_trigger_text(
    trigger: &Trigger,
    msg: &Message,
    bot_nick: &str,
    text: &str,
) -> Option<Vec<String>> {
    let is_privmsg = matches!(msg.command, Command::PRIVMSG(..));
    let msg_target = target_param(msg);
    // A CTCP message is parsed from the text as it arrived. Its payload is
    // protocol data, not chat text, so the formatting codes stay in it. The
    // text of an `ACTION` is chat text and is matched like any other text.
    let ctcp = if is_privmsg {
        CtcpMessage::parse(trailing_param(msg).unwrap_or(""))
    } else {
        None
    };

    match trigger {
        Trigger::Action { pattern, target } => {
            // The command is read from the text as it arrived, so a command
            // that only reads as `ACTION` once the codes are gone, such as
            // "ACT\x02ION", does not fire an action handler. Only the
            // argument of the action is chat text.
            if ctcp.as_ref()?.command != "ACTION" {
                return None;
            }
            if !target_matches(msg_target, target.as_deref()) {
                return None;
            }
            let arg = CtcpMessage::parse(text)?.arg;
            glob_match(pattern, &arg)
        }

        Trigger::Ctcp { command, target } => {
            let ctcp = ctcp?;
            if !ctcp.command.eq_ignore_ascii_case(command) {
                return None;
            }
            if !target_matches(msg_target, target.as_deref()) {
                return None;
            }
            Some(vec![ctcp.arg])
        }

        // Every other trigger matches chat text or other events, never CTCP.
        _ if ctcp.is_some() => None,

        Trigger::Command { name, target, .. } => {
            if !is_privmsg || !target_matches(msg_target, target.as_deref()) {
                return None;
            }
            let text = text.strip_prefix(CMD_PREFIX)?;
            let (cmd, rest) = text
                .split_once(' ')
                .map_or((text, ""), |(c, r)| (c, r.trim()));
            if !cmd.eq_ignore_ascii_case(name) {
                return None;
            }
            Some(if rest.is_empty() {
                vec![]
            } else {
                vec![rest.to_string()]
            })
        }

        Trigger::Message { pattern, target } => {
            if !is_privmsg || !target_matches(msg_target, target.as_deref()) {
                return None;
            }
            glob_match(pattern, text)
        }

        Trigger::Event {
            event,
            target,
            regex,
        } => {
            if !command_name(msg).eq_ignore_ascii_case(event) {
                return None;
            }
            if !target_matches(msg_target, target.as_deref()) {
                return None;
            }
            if let Some(re_str) = regex {
                let re = cached_regex(re_str)?;
                let caps = re.captures(text)?;
                let groups: Vec<String> = caps
                    .iter()
                    .skip(1)
                    .filter_map(|m| m.map(|m| m.as_str().to_string()))
                    .collect();
                Some(groups)
            } else {
                Some(vec![])
            }
        }

        Trigger::Cron { .. } => None,

        Trigger::Mention { target } => {
            if !is_privmsg || !target_matches(msg_target, target.as_deref()) {
                return None;
            }
            let lower = text.to_ascii_lowercase();
            let nick_lower = bot_nick.to_ascii_lowercase();
            // Accept "<nick>: " or "<nick>, " address prefixes.
            // IRC nicks are restricted to ASCII characters (RFC 2812), so
            // `prefix.len()` (bytes) equals its character count and slicing
            // `text` at that offset is always on a valid UTF-8 boundary.
            let rest = [": ", ", "].iter().find_map(|sep| {
                let prefix = format!("{}{}", nick_lower, sep);
                if lower.starts_with(prefix.as_str()) {
                    Some(text[prefix.len()..].trim().to_string())
                } else {
                    None
                }
            })?;
            Some(if rest.is_empty() { vec![] } else { vec![rest] })
        }
    }
}

/// Whether the target of the message satisfies the optional target filter of a
/// trigger. A trigger without a filter takes every target.
fn target_matches(msg_target: Option<&str>, filter: Option<&str>) -> bool {
    match filter {
        Some(t) => msg_target == Some(t),
        None => true,
    }
}

// ─── nick fallback ───────────────────────────────────────────────────────────

/// Generate a fallback nick from `base` for the given 1-based `attempt`, used
/// to recover from `ERR_NICKNAMEINUSE` / `ERR_UNAVAILRESOURCE` during
/// registration.
///
/// The first three attempts append underscores (`base_`, `base__`, `base___`);
/// later attempts append the attempt number (`base4`, `base5`, …) so the
/// candidates stay bounded in length while remaining distinct.
#[must_use]
fn fallback_nick(base: &str, attempt: u32) -> String {
    if attempt <= 3 {
        format!("{base}{}", "_".repeat(attempt as usize))
    } else {
        format!("{base}{attempt}")
    }
}

// ─── IRC message helpers ─────────────────────────────────────────────────────

/// The IRC command name as an uppercase ASCII string (e.g. `"PRIVMSG"`, `"001"`).
///
/// Uses the command's wire representation for known variants, and the stored
/// name directly for `Raw` variants.
fn command_name(msg: &Message) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    match &msg.command {
        Command::Raw(name, _) => Cow::Borrowed(name.as_str()),
        cmd => {
            let s = String::from(cmd);
            let end = s.find(' ').unwrap_or(s.len());
            Cow::Owned(s[..end].to_ascii_uppercase())
        }
    }
}

/// The trailing parameter — the main text content of the message.
fn trailing_param(msg: &Message) -> Option<&str> {
    match &msg.command {
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

/// The first parameter — typically the target channel or nick.
fn target_param(msg: &Message) -> Option<&str> {
    match &msg.command {
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

// ─── regex cache ─────────────────────────────────────────────────────────────

/// Return a clone of the compiled `Regex` for `pattern`, compiling and caching
/// it on the first call with that pattern.
fn cached_regex(pattern: &str) -> Option<Arc<Regex>> {
    static CACHE: OnceLock<RwLock<HashMap<String, Arc<Regex>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));

    // Fast path: pattern already cached.
    if let Ok(guard) = cache.read() {
        if let Some(re) = guard.get(pattern) {
            return Some(Arc::clone(re));
        }
    }

    // Slow path: compile and insert.
    let re = Arc::new(Regex::new(pattern).ok()?);
    if let Ok(mut guard) = cache.write() {
        guard
            .entry(pattern.to_string())
            .or_insert_with(|| Arc::clone(&re));
    }
    Some(re)
}

// ─── glob matching ───────────────────────────────────────────────────────────

/// Match `text` against a glob `pattern` where `*` is a capturing wildcard.
/// Returns `Some(captures)` on success, `None` on mismatch.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> Option<Vec<String>> {
    // Convert glob to a capturing regex and look it up in the cache.
    let re_str = glob_to_regex(pattern);
    let re = cached_regex(&re_str)?;
    let caps = re.captures(text)?;
    let groups: Vec<String> = caps
        .iter()
        .skip(1) // skip whole-match
        .filter_map(|m| m.map(|m| m.as_str().to_string()))
        .collect();
    Some(groups)
}

fn glob_to_regex(pattern: &str) -> String {
    let mut out = String::from("^(?i)");
    for c in pattern.chars() {
        match c {
            '*' => out.push_str("(.*)"),
            '?' => out.push('.'),
            c if ".$+^{}[]|\\()".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('$');
    out
}

// ─── dispatch ────────────────────────────────────────────────────────────────

/// Run the handlers that match `msg`, and return the errors of the handlers
/// that failed.
///
/// The read loop calls this for every line after its own connection work.
/// [`TestBot`](crate::testing::TestBot) calls it for the line of a test, so a
/// test sees the same steps as a live bot. A server `PING` or `PONG` never gets
/// to a handler. The framework answers CTCP `PING` and `VERSION` itself.
pub(crate) async fn handle_message<T: Send + Sync + 'static>(
    bot: &Arc<T>,
    handlers: &HandlerSet<T>,
    msg: &Message,
    bot_nick: &Nick,
    ctcp_version: Option<&str>,
    roles: &[(String, Vec<String>)],
    tx: mpsc::UnboundedSender<String>,
) -> Vec<BoxError> {
    match &msg.command {
        Command::PING(..) | Command::PONG(..) => return Vec::new(),
        Command::PRIVMSG(_, text) => {
            if let Some(ctcp) = CtcpMessage::parse(text) {
                if answer_ctcp(msg, &ctcp, ctcp_version, &tx) {
                    return Vec::new();
                }
            }
        }
        _ => {}
    }
    dispatch(bot, handlers, msg, bot_nick, roles, tx).await
}

/// Answer a CTCP `PING` or `VERSION` from the sender of `msg`. Returns `true`
/// when `ctcp` is one of these commands, and `false` when a handler must get
/// the message.
fn answer_ctcp(
    msg: &Message,
    ctcp: &CtcpMessage,
    ctcp_version: Option<&str>,
    tx: &mpsc::UnboundedSender<String>,
) -> bool {
    let arg = match ctcp.command.as_str() {
        "PING" => ctcp.arg.clone(),
        // Use the caller-supplied version string if set, else the framework
        // default of `ircbot <crate-version>`.
        "VERSION" => ctcp_version.map_or_else(
            || format!("ircbot {}", env!("CARGO_PKG_VERSION")),
            ToString::to_string,
        ),
        _ => return false,
    };
    if let Some(sender) = msg.source_nickname() {
        let reply = ctcp_reply(sender, &ctcp.command, &arg);
        if let Err(e) = tx.send(reply) {
            tracing::error!(command = %ctcp.command, error = %e, "failed to send CTCP reply");
        }
    }
    true
}

/// Build the `NOTICE` line that answers the CTCP `command` from `nick`.
///
/// The nick and the argument are sanitised, because the argument comes from the
/// wire or from the configured version string. A reply longer than the IRC line
/// limit is cut to one line: a CTCP reply split over several lines is several
/// replies.
fn ctcp_reply(nick: &str, command: &str, arg: &str) -> String {
    let arg = sanitize(arg);
    let separator = if arg.is_empty() { "" } else { " " };
    let header = format!("NOTICE {} :\x01{command}{separator}", sanitize(nick));
    make_messages(&header, &arg, "\x01")
        .into_iter()
        .next()
        .unwrap_or_else(|| format!("{header}\x01\r\n"))
}

async fn dispatch<T: Send + Sync + 'static>(
    bot: &Arc<T>,
    handlers: &HandlerSet<T>,
    msg: &Message,
    bot_nick: &Nick,
    roles: &[(String, Vec<String>)],
    tx: mpsc::UnboundedSender<String>,
) -> Vec<BoxError> {
    // Snapshot the current handler list under a brief read-lock, then release
    // immediately — no lock is held across any `.await` point.
    let current: Arc<Vec<HandlerEntry<T>>> = {
        let guard = handlers.read().unwrap_or_else(|e| e.into_inner());
        Arc::clone(&*guard)
    };

    let sender = match msg.prefix.as_ref() {
        Some(Prefix::Nickname(nick, user, host)) if !user.is_empty() => Some(User {
            nick: Nick::from(nick.clone()),
            user: user.clone(),
            host: host.clone(),
        }),
        _ => None,
    };
    let target = Target::from_raw(target_param(msg).unwrap_or(""));

    // The server echoes the bot's own JOIN, PART and NICK back, and with the
    // IRCv3 `echo-message` capability its own PRIVMSG and NOTICE too. Only a
    // handler that asks for them gets these messages.
    let from_self = sender
        .as_ref()
        .is_some_and(|u| nick_eq(u.nick.as_str(), bot_nick.as_str()));

    // Strip the formatting codes once for the whole handler list. Text without
    // a code is borrowed, so the usual message costs no copy.
    let raw_text = trailing_param(msg).unwrap_or("");
    let plain_text = crate::format::strip_cow(raw_text);

    let mut errors = Vec::new();
    for entry in current.iter() {
        if from_self && !entry.include_self {
            continue;
        }
        let text = if entry.raw_text {
            raw_text
        } else {
            plain_text.as_ref()
        };
        if let Some(captures) = check_trigger_text(&entry.trigger, msg, bot_nick.as_str(), text) {
            // Enforce per-command role authorization; unauthorized senders are
            // silently ignored, exactly as if the trigger had not matched.
            if !authorized(roles, &entry.trigger, sender.as_ref()) {
                continue;
            }
            let ctx = Context {
                tx: tx.clone(),
                target: target.clone(),
                sender: sender.clone(),
                raw: msg.clone(),
                bot_nick: bot_nick.clone(),
                captures,
            };
            let bot_clone = Arc::clone(bot);
            let fut = (entry.handler)(bot_clone, ctx);
            if let Err(e) = fut.await {
                errors.push(e);
            }
        }
    }
    errors
}

/// Decide whether `sender` may invoke a handler with the given `trigger`.
///
/// Only [`Trigger::Command`] with `role = Some(_)` is restricted; every other
/// trigger (and any command without a role) is always allowed. A restricted
/// command requires a known `sender` whose `nick!user@host` matches one of the
/// hostmask glob patterns configured for that role (see
/// [`State::with_role`](crate::State::with_role)). A role with no configured
/// patterns — including an unknown role name — authorizes no one.
#[must_use]
pub fn authorized(
    roles: &[(String, Vec<String>)],
    trigger: &Trigger,
    sender: Option<&User>,
) -> bool {
    let Trigger::Command {
        role: Some(required),
        ..
    } = trigger
    else {
        return true;
    };

    let Some(user) = sender else {
        return false;
    };
    let mask = format!("{}!{}@{}", user.nick.as_str(), user.user, user.host);

    roles
        .iter()
        .filter(|(name, _)| name == required)
        .flat_map(|(_, patterns)| patterns)
        .any(|pattern| glob_match(pattern, &mask).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ctcp_reply ─────────────────────────────────────────────────────────────

    #[test]
    fn ctcp_reply_with_argument() {
        assert_eq!(
            ctcp_reply("alice", "PING", "12345"),
            "NOTICE alice :\x01PING 12345\x01\r\n"
        );
    }

    #[test]
    fn ctcp_reply_without_argument_has_no_trailing_space() {
        assert_eq!(
            ctcp_reply("alice", "PING", ""),
            "NOTICE alice :\x01PING\x01\r\n"
        );
    }

    #[test]
    fn ctcp_reply_strips_nul_and_line_breaks() {
        assert_eq!(
            ctcp_reply("alice", "PING", "a\0b\r\nc"),
            "NOTICE alice :\x01PING abc\x01\r\n"
        );
    }

    #[test]
    fn ctcp_reply_is_one_line_within_the_limit() {
        let reply = ctcp_reply("alice", "PING", &"x ".repeat(400));
        assert!(reply.len() <= 512, "reply is {} bytes", reply.len());
        assert_eq!(reply.matches("\r\n").count(), 1);
        assert!(reply.ends_with("\x01\r\n"), "{reply:?}");
    }

    // ── fallback_nick ──────────────────────────────────────────────────────────

    #[test]
    fn fallback_nick_appends_underscores_for_early_attempts() {
        assert_eq!(fallback_nick("bot", 1), "bot_");
        assert_eq!(fallback_nick("bot", 2), "bot__");
        assert_eq!(fallback_nick("bot", 3), "bot___");
    }

    #[test]
    fn fallback_nick_appends_number_for_later_attempts() {
        assert_eq!(fallback_nick("bot", 4), "bot4");
        assert_eq!(fallback_nick("bot", 8), "bot8");
    }

    #[test]
    fn fallback_nick_candidates_are_distinct_across_all_attempts() {
        let mut seen = std::collections::HashSet::new();
        for attempt in 1..=MAX_NICK_ATTEMPTS {
            assert!(
                seen.insert(fallback_nick("bot", attempt)),
                "duplicate fallback nick at attempt {attempt}"
            );
        }
    }

    // ── authorized ───────────────────────────────────────────────────────────

    fn user(nick: &str, u: &str, host: &str) -> User {
        User {
            nick: Nick::from(nick),
            user: u.to_string(),
            host: host.to_string(),
        }
    }

    fn admin_command() -> Trigger {
        Trigger::Command {
            name: "ban".to_string(),
            target: None,
            role: Some("admin".to_string()),
        }
    }

    fn admin_roles() -> Vec<(String, Vec<String>)> {
        vec![("admin".to_string(), vec!["*!*@trusted.host".to_string()])]
    }

    #[test]
    fn authorized_allows_command_without_a_role() {
        let trigger = Trigger::Command {
            name: "ping".to_string(),
            target: None,
            role: None,
        };
        assert!(authorized(&[], &trigger, Some(&user("a", "u", "h"))));
        assert!(authorized(&[], &trigger, None));
    }

    #[test]
    fn authorized_allows_non_command_triggers() {
        let trigger = Trigger::Message {
            pattern: "x".to_string(),
            target: None,
        };
        assert!(authorized(&[], &trigger, None));
    }

    #[test]
    fn authorized_accepts_matching_hostmask() {
        assert!(authorized(
            &admin_roles(),
            &admin_command(),
            Some(&user("alice", "a", "trusted.host"))
        ));
    }

    #[test]
    fn authorized_rejects_non_matching_hostmask() {
        assert!(!authorized(
            &admin_roles(),
            &admin_command(),
            Some(&user("mallory", "m", "evil.host"))
        ));
    }

    #[test]
    fn authorized_rejects_when_sender_is_unknown() {
        assert!(!authorized(&admin_roles(), &admin_command(), None));
    }

    #[test]
    fn authorized_rejects_unknown_role_name() {
        // The command requires "admin", but only "ops" is configured.
        let roles = vec![("ops".to_string(), vec!["*!*@trusted.host".to_string()])];
        assert!(!authorized(
            &roles,
            &admin_command(),
            Some(&user("alice", "a", "trusted.host"))
        ));
    }

    // ── CTCP VERSION ───────────────────────────────────────────────────────────

    /// Drive `handle_message` with a CTCP VERSION request and return the line
    /// the bot would send back.
    async fn ctcp_version_reply(custom: Option<&str>) -> String {
        let bot = std::sync::Arc::new(());
        let handlers = crate::internal::make_handler_set::<()>(vec![]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let msg = ":alice!u@h PRIVMSG mybot :\x01VERSION\x01"
            .parse::<Message>()
            .unwrap();
        let errors =
            handle_message(&bot, &handlers, &msg, &Nick::from("mybot"), custom, &[], tx).await;
        assert!(errors.is_empty());
        rx.try_recv().expect("a CTCP VERSION reply was sent")
    }

    #[tokio::test]
    async fn ctcp_version_uses_custom_string_when_set() {
        assert_eq!(
            ctcp_version_reply(Some("rustbutler 1.2.3")).await,
            "NOTICE alice :\x01VERSION rustbutler 1.2.3\x01\r\n",
        );
    }

    #[tokio::test]
    async fn ctcp_version_defaults_to_ircbot_crate_version() {
        let reply = ctcp_version_reply(None).await;
        assert_eq!(
            reply,
            format!(
                "NOTICE alice :\x01VERSION ircbot {}\x01\r\n",
                env!("CARGO_PKG_VERSION")
            ),
        );
    }

    // ── handle_message ─────────────────────────────────────────────────────────

    /// A handler set with one `#[on(event = …)]` handler that says "fired" and
    /// then returns an error with the text "boom".
    fn failing_event_handler(event: &str) -> HandlerSet<()> {
        crate::internal::make_handler_set(vec![HandlerEntry {
            trigger: Trigger::Event {
                event: event.to_string(),
                target: None,
                regex: None,
            },
            include_self: false,
            raw_text: false,
            handler: Box::new(|_, ctx: Context| {
                Box::pin(async move {
                    ctx.raw("PRIVMSG #chan :fired")?;
                    Err("boom".into())
                })
            }),
        }])
    }

    #[tokio::test]
    async fn handle_message_returns_the_handler_errors() {
        let handlers = failing_event_handler("PRIVMSG");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let msg = ":alice!u@h PRIVMSG #chan :hi".parse::<Message>().unwrap();

        let errors = handle_message(
            &Arc::new(()),
            &handlers,
            &msg,
            &Nick::from("mybot"),
            None,
            &[],
            tx,
        )
        .await;

        let errors: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert_eq!(errors, vec!["boom".to_string()]);
        assert_eq!(rx.try_recv().as_deref(), Ok("PRIVMSG #chan :fired\r\n"));
    }

    #[tokio::test]
    async fn handle_message_does_not_give_a_server_ping_to_handlers() {
        let handlers = failing_event_handler("PING");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let msg = "PING :irc.example.net".parse::<Message>().unwrap();

        let errors = handle_message(
            &Arc::new(()),
            &handlers,
            &msg,
            &Nick::from("mybot"),
            None,
            &[],
            tx,
        )
        .await;

        assert!(errors.is_empty());
        assert!(rx.try_recv().is_err());
    }

    // ── protocol logging ───────────────────────────────────────────────────────

    /// A `tracing` writer that appends everything it is handed to a shared
    /// buffer, so a test can inspect what the subscriber emitted.
    #[derive(Clone, Default)]
    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl CaptureWriter {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap_or_else(|e| e.into_inner()).clone())
                .expect("capture buffer is valid UTF-8")
        }
    }

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Drives the real read/write loop against a local socket that sends a
    /// single `PING` and then disconnects, and asserts that both the inbound
    /// line and the bot's `PONG` reply are emitted on the protocol target.
    #[tokio::test]
    async fn protocol_logging_captures_recv_and_send_on_protocol_target() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let capture = CaptureWriter::default();
        // Capture only the protocol target so unrelated events don't pollute the
        // buffer. Installed for the current thread; `#[tokio::test]` runs a
        // current-thread runtime, so the bot's spawned tasks share it.
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(capture.clone())
            .with_env_filter(format!("{PROTOCOL_LOG_TARGET}=trace"))
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        // A stand-in server: send one PING with a recognisable token, drain the
        // bot's input until its PONG arrives (so the exchange completes), then
        // drop the connection so the bot's read loop tears down.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (read, mut write) = sock.into_split();
            write.write_all(b"PING :capture-token\r\n").await.unwrap();
            write.flush().await.unwrap();
            let mut lines = tokio::io::BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.contains("PONG") {
                    break;
                }
            }
            // Dropping the halves closes the connection.
        });

        let state = State::connect(Nick::from("tester"), &addr.to_string(), vec![])
            .await
            .unwrap();
        let bot = std::sync::Arc::new(());
        let handlers = crate::internal::make_handler_set::<()>(vec![]);

        // The loop returns on its own once the server disconnects (whether
        // cleanly or with a reset); we only care that the exchange was logged.
        // Bound it so a regression can't hang the suite.
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            run_bot_internal(bot, state, handlers),
        )
        .await
        .expect("read loop should return after the server disconnects");

        let out = capture.contents();
        assert!(
            out.contains(PROTOCOL_LOG_TARGET),
            "events should be on the protocol target; got:\n{out}"
        );
        assert!(
            out.contains("recv") && out.contains("PING :capture-token"),
            "the inbound PING should be logged as recv; got:\n{out}"
        );
        assert!(
            out.contains("send") && out.contains("PONG :capture-token"),
            "the outbound PONG should be logged as send; got:\n{out}"
        );
    }
}
