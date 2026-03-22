use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use regex::Regex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufWriter};
use tokio::sync::mpsc;

use crate::{
    connection::State,
    context::{Context, User},
    handler::{HandlerEntry, Trigger},
    irc::{ChannelExt, CtcpMessage, Message},
    BoxError,
};
use irc_proto::{prefix::Prefix, Command, Response};

/// Command prefix recognised by the bot (e.g. `!ping`).
const CMD_PREFIX: char = '!';

/// The token sent in our client-initiated keepalive `PING`.
const KEEPALIVE_TOKEN: &str = "ircbot-keepalive";

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
        keepalive_interval,
        keepalive_timeout,
        flood_burst,
        flood_rate,
        reader,
        write_half,
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

        // Token-bucket state.
        let max_tokens = flood_burst as f64;
        let mut tokens = max_tokens;
        // How fast tokens regenerate: one token per `flood_rate`.
        let token_rate = 1.0 / flood_rate.as_secs_f64(); // tokens per second
        let mut last_refill = tokio::time::Instant::now();

        while let Some(msg) = write_rx.recv().await {
            // Refill tokens based on time elapsed since the last send.
            let now = tokio::time::Instant::now();
            let elapsed = (now - last_refill).as_secs_f64();
            tokens = (tokens + elapsed * token_rate).min(max_tokens);
            last_refill = now;

            // If the bucket is empty, wait until a token becomes available.
            if tokens < 1.0 {
                let wait = Duration::from_secs_f64((1.0 - tokens) / token_rate);
                tokio::time::sleep(wait).await;
                tokens = 0.0;
                last_refill = tokio::time::Instant::now();
            } else {
                tokens -= 1.0;
            }

            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
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
                eprintln!("[ircbot] keepalive timeout — reconnecting");
                if let Some(tx) = fail_tx.take() {
                    let _ = tx.send(());
                }
                break;
            }
        }
    });

    let bot_nick = nick.clone();
    let mut joined = false;
    let mut lines = reader.lines();
    let mut keepalive_fail_rx = keepalive_fail_rx;

    // Run the read loop; collect any IO error so we can clean up first.
    let loop_result: Result<(), BoxError> = async {
        loop {
            tokio::select! {
                result = lines.next_line() => {
                    let Some(line) = result? else { break; };
                    let line = line.trim_end_matches('\r').to_string();
                    if line.is_empty() {
                        continue;
                    }

                    if let Ok(msg) = line.parse::<Message>() {
                        match &msg.command {
                            Command::PING(srv, _) => {
                                if let Err(e) = write_tx.send(format!("PONG :{srv}\r\n")) {
                                    eprintln!("[ircbot] failed to send PONG: {e}");
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
                                if !joined {
                                    joined = true;
                                    for ch in &channels {
                                        if let Err(e) = write_tx.send(format!("JOIN {ch}\r\n")) {
                                            eprintln!("[ircbot] failed to send JOIN {ch}: {e}");
                                        }
                                    }
                                }
                                dispatch(&bot, &handlers, &msg, &bot_nick, write_tx.clone()).await;
                            }
                            Command::PRIVMSG(_, _) => {
                                handle_privmsg(
                                    &bot,
                                    &handlers,
                                    &msg,
                                    &bot_nick,
                                    write_tx.clone(),
                                )
                                .await;
                            }
                            _ => {
                                dispatch(&bot, &handlers, &msg, &bot_nick, write_tx.clone()).await;
                            }
                        }
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

    // Always clean up the keepalive and write tasks before returning.
    keepalive_task.abort();
    drop(write_tx);
    let _ = write_task.await;

    loop_result
}

// ─── trigger matching ────────────────────────────────────────────────────────

/// Returns `Some(captures)` if `msg` matches `trigger`, `None` otherwise.
#[must_use]
pub fn check_trigger(trigger: &Trigger, msg: &Message, bot_nick: &str) -> Option<Vec<String>> {
    match trigger {
        Trigger::Command { name, target } => {
            let Command::PRIVMSG(msg_target, text) = &msg.command else {
                return None;
            };
            // Optional target filter
            if let Some(t) = target {
                if msg_target.as_str() != t.as_str() {
                    return None;
                }
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
            let Command::PRIVMSG(msg_target, text) = &msg.command else {
                return None;
            };
            if let Some(t) = target {
                if msg_target.as_str() != t.as_str() {
                    return None;
                }
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
            if let Some(t) = target {
                if target_param(msg) != Some(t.as_str()) {
                    return None;
                }
            }
            if let Some(re_str) = regex {
                let text = trailing_param(msg).unwrap_or("");
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

        Trigger::Mention { target } => {
            let Command::PRIVMSG(msg_target, text) = &msg.command else {
                return None;
            };
            if let Some(t) = target {
                if msg_target.as_str() != t.as_str() {
                    return None;
                }
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

async fn handle_privmsg<T: Send + Sync + 'static>(
    bot: &Arc<T>,
    handlers: &HandlerSet<T>,
    msg: &Message,
    bot_nick: &str,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    let Command::PRIVMSG(_, text) = &msg.command else {
        dispatch(bot, handlers, msg, bot_nick, tx).await;
        return;
    };
    if let Some(ctcp) = CtcpMessage::parse(text) {
        match ctcp.command.as_str() {
            "PING" => {
                if let Some(sender) = msg.source_nickname() {
                    let reply = format!(
                        "NOTICE {sender} :\x01PING{}{}\x01\r\n",
                        if ctcp.arg.is_empty() { "" } else { " " },
                        ctcp.arg,
                    );
                    if let Err(e) = tx.send(reply) {
                        eprintln!("[ircbot] failed to send CTCP PING reply: {e}");
                    }
                }
                return;
            }
            "VERSION" => {
                if let Some(sender) = msg.source_nickname() {
                    let reply = format!(
                        "NOTICE {sender} :\x01VERSION ircbot {}\x01\r\n",
                        env!("CARGO_PKG_VERSION"),
                    );
                    if let Err(e) = tx.send(reply) {
                        eprintln!("[ircbot] failed to send CTCP VERSION reply: {e}");
                    }
                }
                return;
            }
            _ => {}
        }
    }
    dispatch(bot, handlers, msg, bot_nick, tx).await;
}

async fn dispatch<T: Send + Sync + 'static>(
    bot: &Arc<T>,
    handlers: &HandlerSet<T>,
    msg: &Message,
    bot_nick: &str,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    // Snapshot the current handler list under a brief read-lock, then release
    // immediately — no lock is held across any `.await` point.
    let current: Arc<Vec<HandlerEntry<T>>> = {
        let guard = handlers.read().unwrap_or_else(|e| e.into_inner());
        Arc::clone(&*guard)
    };

    let sender = match msg.prefix.as_ref() {
        Some(Prefix::Nickname(nick, user, host)) if !user.is_empty() => Some(User {
            nick: nick.clone(),
            user: user.clone(),
            host: host.clone(),
        }),
        _ => None,
    };
    let target = target_param(msg).unwrap_or("").to_string();
    let is_channel = target.is_channel_name();

    for entry in current.iter() {
        if let Some(captures) = check_trigger(&entry.trigger, msg, bot_nick) {
            let ctx = Context {
                tx: tx.clone(),
                target: target.clone(),
                is_channel,
                sender: sender.clone(),
                raw: msg.clone(),
                bot_nick: bot_nick.to_string(),
                captures,
            };
            let bot_clone = Arc::clone(bot);
            let fut = (entry.handler)(bot_clone, ctx);
            if let Err(e) = fut.await {
                eprintln!("[ircbot] handler error: {e}");
            }
        }
    }
}
