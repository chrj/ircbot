//! Tests for the ignore list: a message from a sender that matches a hostmask
//! mask reaches no handler, and gets no CTCP reply.
//!
//! Run with:
//!   cargo test --test ignore

use ircbot::testing::TestBot;
use ircbot::{bot, Context, Result, User};

// ─── bot under test ──────────────────────────────────────────────────────────

#[bot]
impl QuietBot {
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.say("pong")
    }

    // The pattern avoids "!ping", so a command test sees one reply only.
    #[on(message = "hello*")]
    async fn echo(&self, ctx: Context) -> Result {
        ctx.say("heard hello")
    }

    #[on(event = "TOPIC")]
    async fn topic(&self, ctx: Context) -> Result {
        ctx.say("topic changed")
    }

    #[on(event = "JOIN")]
    async fn welcome(&self, ctx: Context, user: User) -> Result {
        ctx.say(format!("welcome {}", user.nick))
    }
}

/// A bot that ignores one host and one nick.
fn quiet_bot() -> TestBot<QuietBot> {
    TestBot::new(QuietBot::default())
        .with_nick("mybot")
        .with_ignore(["*!*@spam.example", "otherbot!*@*"])
}

/// The replies for `line`, or a panic with the error of the delivery.
async fn replies(bot: &TestBot<QuietBot>, line: &str) -> Vec<String> {
    bot.deliver(line).await.expect("the delivery succeeds")
}

// ── a sender that is not ignored ─────────────────────────────────────────────

#[tokio::test]
async fn a_command_of_another_user_still_fires() {
    let got = replies(&quiet_bot(), ":alice!a@good.host PRIVMSG #chan :!ping").await;
    assert_eq!(got, vec!["PRIVMSG #chan :pong\r\n"]);
}

#[tokio::test]
async fn a_bot_without_masks_answers_everyone() {
    let bot = TestBot::new(QuietBot::default()).with_nick("mybot");
    let got = replies(&bot, ":spammer!s@spam.example PRIVMSG #chan :!ping").await;
    assert_eq!(got, vec!["PRIVMSG #chan :pong\r\n"]);
}

// ── an ignored sender ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_command_of_an_ignored_host_does_not_fire() {
    let got = replies(&quiet_bot(), ":spammer!s@spam.example PRIVMSG #chan :!ping").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

#[tokio::test]
async fn text_of_an_ignored_nick_does_not_fire() {
    let got = replies(&quiet_bot(), ":otherbot!o@any.host PRIVMSG #chan :hello").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

#[tokio::test]
async fn an_ignored_nick_is_matched_without_case() {
    let got = replies(&quiet_bot(), ":OtherBot!o@any.host PRIVMSG #chan :hello").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

#[tokio::test]
async fn an_event_of_an_ignored_sender_does_not_fire() {
    let got = replies(&quiet_bot(), ":spammer!s@spam.example JOIN #chan").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

// ── CTCP ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_ignored_sender_gets_no_ctcp_version_reply() {
    let got = replies(
        &quiet_bot(),
        ":spammer!s@spam.example PRIVMSG mybot :\x01VERSION\x01",
    )
    .await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

#[tokio::test]
async fn an_ignored_sender_gets_no_ctcp_ping_reply() {
    let got = replies(
        &quiet_bot(),
        ":spammer!s@spam.example PRIVMSG mybot :\x01PING 1234\x01",
    )
    .await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

#[tokio::test]
async fn another_sender_still_gets_a_ctcp_version_reply() {
    let got = replies(
        &quiet_bot(),
        ":alice!a@good.host PRIVMSG mybot :\x01VERSION\x01",
    )
    .await;
    assert_eq!(got.len(), 1, "expected one reply, got {got:?}");
    assert!(
        got[0].starts_with("NOTICE alice :\x01VERSION ircbot "),
        "unexpected reply: {:?}",
        got[0]
    );
}

// ── messages without a sender ────────────────────────────────────────────────

#[tokio::test]
async fn a_message_from_the_server_is_not_ignored() {
    // The masks name users, so a server message passes even with "*!*@*".
    let bot = TestBot::new(QuietBot::default())
        .with_nick("mybot")
        .with_ignore(["*!*@*"]);
    let got = replies(&bot, ":irc.example.net TOPIC #chan :new topic").await;
    assert_eq!(got, vec!["PRIVMSG #chan :topic changed\r\n"]);
}
