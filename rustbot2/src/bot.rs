use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use regex::Regex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufWriter};
use tokio::sync::mpsc;

use crate::{
    connection::BotState,
    context::Context,
    handler::{HandlerEntry, Trigger},
    irc::{is_channel_name, IrcMessage},
    BoxError,
};

/// Command prefix recognised by the bot (e.g. `!ping`).
const CMD_PREFIX: char = '!';

// ─── public entry-point ──────────────────────────────────────────────────────

/// Handles IRC messages, dispatching to registered handlers.
///
/// # Errors
///
/// Returns an error if reading from the connection fails.
pub async fn run_bot_internal<T: Send + Sync + 'static>(
    bot: Arc<T>,
    state: BotState,
    handlers: Vec<HandlerEntry<T>>,
) -> Result<(), BoxError> {
    let BotState {
        nick,
        channels,
        reader,
        write_half,
    } = state;

    // Create the mpsc write channel.
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();

    // Spawn the write loop — drains the channel into the TCP write half.
    let write_task = tokio::spawn(async move {
        let mut writer = BufWriter::new(write_half);
        while let Some(msg) = write_rx.recv().await {
            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    });

    let handlers = Arc::new(handlers);
    let bot_nick = nick.clone();
    let mut joined = false;
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim_end_matches('\r').to_string();
        if line.is_empty() {
            continue;
        }

        if let Some(msg) = IrcMessage::parse(&line) {
            match msg.command.as_str() {
                "PING" => {
                    let srv = msg.params.first().map_or("", String::as_str);
                    if let Err(e) = write_tx.send(format!("PONG :{srv}\r\n")) {
                        eprintln!("[rustbot2] failed to send PONG: {e}");
                    }
                }
                "001" => {
                    if !joined {
                        joined = true;
                        for ch in &channels {
                            if let Err(e) = write_tx.send(format!("JOIN {ch}\r\n")) {
                                eprintln!("[rustbot2] failed to send JOIN {ch}: {e}");
                            }
                        }
                    }
                }
                _ => {
                    dispatch(&bot, &handlers, &msg, &bot_nick, write_tx.clone()).await;
                }
            }
        }
    }

    // Close the write channel so the write task drains any pending messages and exits.
    drop(write_tx);
    let _ = write_task.await;
    Ok(())
}

// ─── trigger matching ────────────────────────────────────────────────────────

/// Returns `Some(captures)` if `msg` matches `trigger`, `None` otherwise.
#[must_use]
pub fn check_trigger(trigger: &Trigger, msg: &IrcMessage, bot_nick: &str) -> Option<Vec<String>> {
    match trigger {
        Trigger::Command { name, target } => {
            if msg.command != "PRIVMSG" {
                return None;
            }
            // Optional target filter
            if let Some(t) = target {
                if msg.target() != Some(t.as_str()) {
                    return None;
                }
            }
            let text = msg.trailing()?;
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
            if msg.command != "PRIVMSG" {
                return None;
            }
            if let Some(t) = target {
                if msg.target() != Some(t.as_str()) {
                    return None;
                }
            }
            let text = msg.trailing()?;
            glob_match(pattern, text)
        }

        Trigger::Event {
            event,
            target,
            regex,
        } => {
            if !msg.command.eq_ignore_ascii_case(event) {
                return None;
            }
            if let Some(t) = target {
                if msg.target() != Some(t.as_str()) {
                    return None;
                }
            }
            if let Some(re_str) = regex {
                let text = msg.trailing().unwrap_or("");
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
            if msg.command != "PRIVMSG" {
                return None;
            }
            if let Some(t) = target {
                if msg.target() != Some(t.as_str()) {
                    return None;
                }
            }
            let text = msg.trailing()?;
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

async fn dispatch<T: Send + Sync + 'static>(
    bot: &Arc<T>,
    handlers: &Arc<Vec<HandlerEntry<T>>>,
    msg: &IrcMessage,
    bot_nick: &str,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    let sender = msg.parse_user();
    let target = msg.target().unwrap_or("").to_string();
    let is_channel = is_channel_name(&target);

    for entry in handlers.as_slice() {
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
                eprintln!("[rustbot2] handler error: {e}");
            }
        }
    }
}
