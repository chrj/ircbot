//! Tests that the triggers match the text without the IRC formatting codes,
//! and that the `raw` option keeps the codes.
//!
//! Run with:
//!   cargo test --test plain_matching

use ircbot::testing::TestBot;
use ircbot::{bot, Context, Result};

// ─── bot under test ──────────────────────────────────────────────────────────

#[bot]
impl FormatBot {
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.say("pong")
    }

    #[command("say")]
    async fn say(&self, ctx: Context, text: String) -> Result {
        ctx.say(format!("said {text}"))
    }

    #[on(message = "seen *")]
    async fn seen(&self, ctx: Context, who: String) -> Result {
        ctx.say(format!("seen {who}"))
    }

    #[on(message = "* kroner")]
    async fn price(&self, ctx: Context, amount: String) -> Result {
        ctx.say(format!("price {amount}"))
    }

    #[on(mention)]
    async fn mention(&self, ctx: Context, text: String) -> Result {
        ctx.say(format!("mention {text}"))
    }

    #[on(action = "waves at *")]
    async fn wave(&self, ctx: Context, who: String) -> Result {
        ctx.say(format!("wave {who}"))
    }

    #[on(event = "TOPIC", regex = "^release (.+)$")]
    async fn topic(&self, ctx: Context, version: String) -> Result {
        ctx.say(format!("topic {version}"))
    }

    #[on(message = "raw *", raw)]
    async fn raw_message(&self, ctx: Context, rest: String) -> Result {
        ctx.say(format!("raw {}", rest.escape_debug()))
    }

    // A pattern that only the text with its codes can match.
    #[on(message = "bold \x02*\x02", raw)]
    async fn bold_only(&self, ctx: Context, who: String) -> Result {
        ctx.say(format!("bold {who}"))
    }
}

fn format_bot() -> TestBot<FormatBot> {
    TestBot::new(FormatBot::default()).with_nick("mybot")
}

/// The replies for `line`, or a panic with the error of the delivery.
async fn replies(bot: &TestBot<FormatBot>, line: &str) -> Vec<String> {
    bot.deliver(line).await.expect("the delivery succeeds")
}

// ── commands ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_command_in_bold_fires() {
    let got = replies(&format_bot(), ":alice!a@h PRIVMSG #chan :\x02!ping\x02").await;
    assert_eq!(got, vec!["PRIVMSG #chan :pong\r\n"]);
}

#[tokio::test]
async fn a_command_argument_loses_the_codes() {
    let got = replies(
        &format_bot(),
        ":alice!a@h PRIVMSG #chan :!say \x02hello\x02",
    )
    .await;
    assert_eq!(got, vec!["PRIVMSG #chan :said hello\r\n"]);
}

// ── message globs ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_glob_matches_text_in_colour() {
    let got = replies(
        &format_bot(),
        ":alice!a@h PRIVMSG #chan :\x0304seen bob\x03",
    )
    .await;
    assert_eq!(got, vec!["PRIVMSG #chan :seen bob\r\n"]);
}

#[tokio::test]
async fn a_capture_carries_the_text_without_the_codes() {
    let got = replies(&format_bot(), ":alice!a@h PRIVMSG #chan :seen \x02bob\x02").await;
    assert_eq!(got, vec!["PRIVMSG #chan :seen bob\r\n"]);
}

#[tokio::test]
async fn digits_after_a_colour_stay_in_the_capture() {
    // "\x0312" is colour 12; "345 kroner" is the text.
    let got = replies(&format_bot(), ":alice!a@h PRIVMSG #chan :\x0312345 kroner").await;
    assert_eq!(got, vec!["PRIVMSG #chan :price 345\r\n"]);
}

// ── mention ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_mention_in_colour_fires() {
    let got = replies(
        &format_bot(),
        ":alice!a@h PRIVMSG #chan :\x0304mybot:\x03 hi",
    )
    .await;
    assert_eq!(got, vec!["PRIVMSG #chan :mention hi\r\n"]);
}

// ── action and event ─────────────────────────────────────────────────────────

#[tokio::test]
async fn an_action_in_bold_fires() {
    let got = replies(
        &format_bot(),
        ":alice!a@h PRIVMSG #chan :\x01ACTION waves at \x02bob\x02\x01",
    )
    .await;
    assert_eq!(got, vec!["PRIVMSG #chan :wave bob\r\n"]);
}

#[tokio::test]
async fn an_event_regex_matches_the_text_without_codes() {
    let got = replies(
        &format_bot(),
        ":alice!a@h TOPIC #chan :release \x021.2.3\x02",
    )
    .await;
    assert_eq!(got, vec!["PRIVMSG #chan :topic 1.2.3\r\n"]);
}

// ── the raw option ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_raw_handler_gets_the_codes() {
    let got = replies(&format_bot(), ":alice!a@h PRIVMSG #chan :raw \x02bold\x02").await;
    assert_eq!(got, vec!["PRIVMSG #chan :raw \\u{2}bold\\u{2}\r\n"]);
}

#[tokio::test]
async fn a_raw_pattern_matches_the_codes_themselves() {
    // The pattern carries the codes, so it matches only the raw text.
    let got = replies(&format_bot(), ":alice!a@h PRIVMSG #chan :bold \x02bob\x02").await;
    assert_eq!(got, vec!["PRIVMSG #chan :bold bob\r\n"]);
}

#[tokio::test]
async fn a_raw_pattern_does_not_match_text_without_codes() {
    let got = replies(&format_bot(), ":alice!a@h PRIVMSG #chan :bold bob").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}

// ── text without codes is unaffected ─────────────────────────────────────────

#[tokio::test]
async fn plain_text_still_matches() {
    let got = replies(&format_bot(), ":alice!a@h PRIVMSG #chan :seen bob").await;
    assert_eq!(got, vec!["PRIVMSG #chan :seen bob\r\n"]);
}

#[tokio::test]
async fn text_that_matches_no_pattern_gives_no_replies() {
    let got = replies(&format_bot(), ":alice!a@h PRIVMSG #chan :\x02just talk\x02").await;
    assert!(got.is_empty(), "unexpected replies: {got:?}");
}
