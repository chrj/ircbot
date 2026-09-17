//! Tests for the `scope` option, which limits a handler to a channel or to a
//! private message.
//!
//! Run with:
//!   cargo test --test scope

use ircbot::testing::TestBot;
use ircbot::{bot, Context, Result};

// ─── bot under test ──────────────────────────────────────────────────────────

#[bot]
impl ScopeBot {
    #[on(mention)]
    async fn anywhere(&self, ctx: Context, text: String) -> Result {
        ctx.say(format!("anywhere {text}"))
    }

    #[on(message = "hello*", scope = "channel")]
    async fn in_channel(&self, ctx: Context) -> Result {
        ctx.say("in channel")
    }

    #[on(message = "hello*", scope = "private")]
    async fn in_query(&self, ctx: Context) -> Result {
        ctx.say("in query")
    }

    #[command("shutdown", scope = "private")]
    async fn shutdown(&self, ctx: Context) -> Result {
        ctx.say("shutting down")
    }

    #[command("topic", scope = "channel", target = "#ops")]
    async fn topic(&self, ctx: Context) -> Result {
        ctx.say("topic")
    }

    #[on(event = "JOIN", scope = "channel")]
    async fn joined(&self, ctx: Context) -> Result {
        ctx.say("joined")
    }

    #[on(event = "QUIT", scope = "channel")]
    async fn quit_in_channel(&self, ctx: Context) -> Result {
        ctx.say("quit")
    }
}

fn scope_bot() -> TestBot<ScopeBot> {
    TestBot::new(ScopeBot::default()).with_nick("mybot")
}

/// The replies for `line`, or a panic with the error of the delivery.
async fn replies(bot: &TestBot<ScopeBot>, line: &str) -> Vec<String> {
    bot.deliver(line).await.expect("the delivery succeeds")
}

// ── a handler without the option ─────────────────────────────────────────────

#[tokio::test]
async fn a_handler_without_a_scope_answers_in_a_channel() {
    let got = replies(&scope_bot(), ":alice!a@h PRIVMSG #chan :mybot: hi").await;
    assert_eq!(got, vec!["PRIVMSG #chan :anywhere hi\r\n"]);
}

#[tokio::test]
async fn a_handler_without_a_scope_answers_a_private_message() {
    let got = replies(&scope_bot(), ":alice!a@h PRIVMSG mybot :mybot: hi").await;
    assert_eq!(got, vec!["PRIVMSG alice :anywhere hi\r\n"]);
}

// ── scope = "channel" ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_channel_handler_fires_in_a_channel() {
    let got = replies(&scope_bot(), ":alice!a@h PRIVMSG #chan :hello").await;
    assert_eq!(got, vec!["PRIVMSG #chan :in channel\r\n"]);
}

#[tokio::test]
async fn a_channel_handler_does_not_fire_in_a_query() {
    let got = replies(&scope_bot(), ":alice!a@h PRIVMSG mybot :hello").await;
    assert_eq!(got, vec!["PRIVMSG alice :in query\r\n"]);
}

// ── scope = "private" ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_private_command_fires_in_a_query() {
    let got = replies(&scope_bot(), ":alice!a@h PRIVMSG mybot :!shutdown").await;
    assert_eq!(got, vec!["PRIVMSG alice :shutting down\r\n"]);
}

#[tokio::test]
async fn a_private_command_does_not_fire_in_a_channel() {
    let got = replies(&scope_bot(), ":alice!a@h PRIVMSG #chan :!shutdown").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

// ── scope with target ────────────────────────────────────────────────────────

#[tokio::test]
async fn scope_and_target_must_both_match() {
    let bot = scope_bot();
    let got = replies(&bot, ":alice!a@h PRIVMSG #ops :!topic").await;
    assert_eq!(got, vec!["PRIVMSG #ops :topic\r\n"]);

    // The right scope, but another channel.
    let got = replies(&bot, ":alice!a@h PRIVMSG #chan :!topic").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");

    // The right name, but a query.
    let got = replies(&bot, ":alice!a@h PRIVMSG mybot :!topic").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

// ── events ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_channel_scope_fires_for_a_join() {
    let got = replies(&scope_bot(), ":alice!a@h JOIN #chan").await;
    assert_eq!(got, vec!["PRIVMSG #chan :joined\r\n"]);
}

#[tokio::test]
async fn an_event_without_a_target_reaches_no_handler_with_a_scope() {
    // A QUIT names no channel or nick, so it belongs to neither scope.
    let got = replies(&scope_bot(), ":alice!a@h QUIT :bye").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}
