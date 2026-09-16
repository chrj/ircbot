//! Tests for `ircbot::testing::TestBot`: a raw IRC line goes through the
//! trigger match, the role check, and the wrappers that `#[bot]` generates.
//!
//! Run with:
//!   cargo test --test test_bot

use ircbot::testing::{DeliverError, TestBot};
use ircbot::{bot, Context, Result};

// ─── bot under test ──────────────────────────────────────────────────────────

#[bot]
impl EchoBot {
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.say("pong")
    }

    #[command("add")]
    async fn add(&self, ctx: Context, a: i64, b: i64) -> Result {
        ctx.reply(a + b)
    }

    #[command("op", role = "admin")]
    async fn op(&self, ctx: Context) -> Result {
        ctx.say("opped")
    }

    #[command("fail")]
    async fn fail(&self, ctx: Context) -> Result {
        ctx.say("about to fail")?;
        Err("the fail command always fails".into())
    }

    #[on(mention)]
    async fn mention(&self, ctx: Context, text: String) -> Result {
        ctx.say(format!("mention: {text}"))
    }

    #[on(event = "PRIVMSG", target = "#log")]
    async fn log(&self, ctx: Context) -> Result {
        ctx.say(format!("text: {}", ctx.message_text()))
    }

    #[on(action = "waves at *", target = "#log")]
    async fn wave(&self, ctx: Context, who: String) -> Result {
        ctx.say(format!("wave: {who}"))
    }

    #[on(event = "PING")]
    async fn server_ping(&self, ctx: Context) -> Result {
        ctx.raw("PRIVMSG #chan :server ping")
    }
}

fn echo_bot() -> TestBot<EchoBot> {
    TestBot::new(EchoBot::default())
}

/// The replies for `line`, or a panic with the error of the delivery.
async fn replies(bot: &TestBot<EchoBot>, line: &str) -> Vec<String> {
    bot.deliver(line).await.expect("the delivery succeeds")
}

// ── commands ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn command_reply_is_returned_as_a_wire_line() {
    let got = replies(&echo_bot(), ":alice!a@host PRIVMSG #chan :!ping").await;
    assert_eq!(got, vec!["PRIVMSG #chan :pong\r\n"]);
}

#[tokio::test]
async fn command_arguments_are_parsed_into_typed_parameters() {
    let got = replies(&echo_bot(), ":alice!a@host PRIVMSG #chan :!add 2 3").await;
    assert_eq!(got, vec!["PRIVMSG #chan :alice, 5\r\n"]);
}

#[tokio::test]
async fn command_with_a_bad_argument_replies_with_the_usage() {
    let got = replies(&echo_bot(), ":alice!a@host PRIVMSG #chan :!add two 3").await;
    assert_eq!(got, vec!["PRIVMSG #chan :alice, usage: !add <a> <b>\r\n"]);
}

// ── roles ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn role_command_fires_for_a_matching_hostmask() {
    let bot = echo_bot().with_role("admin", ["*!*@trusted.host"]);
    let got = replies(&bot, ":alice!a@trusted.host PRIVMSG #chan :!op").await;
    assert_eq!(got, vec!["PRIVMSG #chan :opped\r\n"]);
}

#[tokio::test]
async fn role_command_is_ignored_for_another_host() {
    let bot = echo_bot().with_role("admin", ["*!*@trusted.host"]);
    let got = replies(&bot, ":mallory!m@other.host PRIVMSG #chan :!op").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

// ── mention ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn mention_handler_gets_the_text_without_the_address_prefix() {
    let bot = echo_bot().with_nick("mybot");
    let got = replies(&bot, ":alice!a@host PRIVMSG #chan :mybot: yes, indeed").await;
    assert_eq!(got, vec!["PRIVMSG #chan :mention: yes, indeed\r\n"]);
}

#[tokio::test]
async fn mention_of_another_nick_gives_no_replies() {
    let bot = echo_bot().with_nick("mybot");
    let got = replies(&bot, ":alice!a@host PRIVMSG #chan :otherbot: hi").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

// ── CTCP ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn plain_text_reaches_a_privmsg_event_handler() {
    let got = replies(&echo_bot(), ":alice!a@host PRIVMSG #log :hello").await;
    assert_eq!(got, vec!["PRIVMSG #log :text: hello\r\n"]);
}

#[tokio::test]
async fn action_reaches_the_action_handler_but_not_the_privmsg_event_handler() {
    let got = replies(
        &echo_bot(),
        ":alice!a@host PRIVMSG #log :\x01ACTION waves at bob\x01",
    )
    .await;
    assert_eq!(got, vec!["PRIVMSG #log :wave: bob\r\n"]);
}

#[tokio::test]
async fn ctcp_version_is_answered_with_the_configured_version() {
    let bot = echo_bot().with_ctcp_version("echobot 1.0");
    let got = replies(&bot, ":alice!a@host PRIVMSG testbot :\x01VERSION\x01").await;
    assert_eq!(got, vec!["NOTICE alice :\x01VERSION echobot 1.0\x01\r\n"]);
}

// ── other lines ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn server_ping_does_not_reach_handlers() {
    let got = replies(&echo_bot(), "PING :irc.example.net").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

#[tokio::test]
async fn line_that_matches_no_trigger_gives_no_replies() {
    let got = replies(&echo_bot(), ":alice!a@host PRIVMSG #chan :just chatting").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

#[tokio::test]
async fn line_with_crlf_at_the_end_is_accepted() {
    let got = replies(&echo_bot(), ":alice!a@host PRIVMSG #chan :!ping\r\n").await;
    assert_eq!(got, vec!["PRIVMSG #chan :pong\r\n"]);
}

#[tokio::test]
async fn replies_of_one_delivery_are_not_in_the_next() {
    let bot = echo_bot();
    let _ = replies(&bot, ":alice!a@host PRIVMSG #chan :!ping").await;
    let got = replies(&bot, ":alice!a@host PRIVMSG #chan :just chatting").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

// ── errors ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn handler_error_is_returned_with_the_replies() {
    let err = echo_bot()
        .deliver(":alice!a@host PRIVMSG #chan :!fail")
        .await
        .expect_err("the fail command returns an error");

    let DeliverError::Handler { errors, replies } = err else {
        panic!("expected DeliverError::Handler, got {err:?}");
    };
    let errors: Vec<String> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(errors, vec!["the fail command always fails"]);
    assert_eq!(replies, vec!["PRIVMSG #chan :about to fail\r\n"]);
}

#[tokio::test]
async fn empty_line_is_an_invalid_line() {
    let err = echo_bot()
        .deliver("\r\n")
        .await
        .expect_err("an empty line is not an IRC message");

    let DeliverError::InvalidLine { line, .. } = err else {
        panic!("expected DeliverError::InvalidLine, got {err:?}");
    };
    assert_eq!(line, "");
}
