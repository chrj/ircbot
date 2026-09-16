//! Tests for the rule that a handler does not get the messages of the bot
//! itself, and for the `include_self` option that turns the rule off.
//!
//! Run with:
//!   cargo test --test own_messages

use ircbot::testing::TestBot;
use ircbot::{bot, Context, Result, User};

// ─── bot under test ──────────────────────────────────────────────────────────

#[bot]
impl JoinBot {
    #[on(event = "JOIN")]
    async fn welcome(&self, ctx: Context, user: User) -> Result {
        ctx.say(format!("welcome {}", user.nick))
    }

    #[on(event = "JOIN", include_self)]
    async fn track(&self, ctx: Context, user: User) -> Result {
        ctx.say(format!("track {}", user.nick))
    }

    #[on(message = "*")]
    async fn echo(&self, ctx: Context, text: String) -> Result {
        ctx.say(format!("heard {text}"))
    }

    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.say("pong")
    }

    #[command("count", include_self)]
    async fn count(&self, ctx: Context) -> Result {
        ctx.say("counted")
    }
}

/// A `TestBot` whose nick is `"mybot"`.
fn join_bot() -> TestBot<JoinBot> {
    TestBot::new(JoinBot::default()).with_nick("mybot")
}

/// The replies for `line`, or a panic with the error of the delivery.
async fn replies(bot: &TestBot<JoinBot>, line: &str) -> Vec<String> {
    bot.deliver(line).await.expect("the delivery succeeds")
}

// ── messages of another user ─────────────────────────────────────────────────

#[tokio::test]
async fn join_of_another_user_reaches_every_handler() {
    let got = replies(&join_bot(), ":alice!a@host JOIN #chan").await;
    assert_eq!(
        got,
        vec![
            "PRIVMSG #chan :welcome alice\r\n",
            "PRIVMSG #chan :track alice\r\n",
        ]
    );
}

#[tokio::test]
async fn text_of_another_user_reaches_the_handler() {
    let got = replies(&join_bot(), ":alice!a@host PRIVMSG #chan :hello").await;
    assert_eq!(got, vec!["PRIVMSG #chan :heard hello\r\n"]);
}

// ── messages of the bot itself ───────────────────────────────────────────────

#[tokio::test]
async fn own_join_reaches_only_the_handler_with_include_self() {
    let got = replies(&join_bot(), ":mybot!b@host JOIN #chan").await;
    assert_eq!(got, vec!["PRIVMSG #chan :track mybot\r\n"]);
}

#[tokio::test]
async fn own_join_is_matched_without_case() {
    let got = replies(&join_bot(), ":MyBot!b@host JOIN #chan").await;
    assert_eq!(got, vec!["PRIVMSG #chan :track MyBot\r\n"]);
}

#[tokio::test]
async fn own_text_does_not_reach_the_handler() {
    let got = replies(&join_bot(), ":mybot!b@host PRIVMSG #chan :hello").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

#[tokio::test]
async fn own_command_does_not_reach_the_handler() {
    let got = replies(&join_bot(), ":mybot!b@host PRIVMSG #chan :!ping").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

#[tokio::test]
async fn own_command_reaches_a_command_with_include_self() {
    let got = replies(&join_bot(), ":mybot!b@host PRIVMSG #chan :!count").await;
    assert_eq!(got, vec!["PRIVMSG #chan :counted\r\n"]);
}

// ── messages of a user with a similar nick ───────────────────────────────────

#[tokio::test]
async fn a_nick_that_starts_with_the_bot_nick_is_another_user() {
    let got = replies(&join_bot(), ":mybot2!b@host JOIN #chan").await;
    assert_eq!(
        got,
        vec![
            "PRIVMSG #chan :welcome mybot2\r\n",
            "PRIVMSG #chan :track mybot2\r\n",
        ]
    );
}
