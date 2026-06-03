use ircbot::{bot, Context, Result};

#[bot]
impl MyBot {
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.reply("pong!")
    }

    #[command("echo")]
    async fn echo(&self, ctx: Context, text: String) -> Result {
        ctx.say(text)
    }

    #[on(message = "hello *")]
    async fn greet(&self, ctx: Context, who: String) -> Result {
        ctx.say(format!("Hello, {who}!"))
    }

    #[on(event = "JOIN", target = "#rust")]
    async fn welcome(&self, ctx: Context) -> Result {
        if let Some(user) = &ctx.sender {
            ctx.say(format!("Welcome to #rust, {}!", user.nick))
        } else {
            Ok(())
        }
    }

    /// Fires when someone addresses the bot by name, e.g. "rustbot: hello".
    #[on(mention)]
    async fn on_mention(&self, ctx: Context, text: String) -> Result {
        ctx.reply(format!("You said: {}", text))
    }

    /// Same as above but only in a specific channel.
    #[on(mention, target = "#rust")]
    async fn on_mention_rust(&self, ctx: Context) -> Result {
        ctx.notice("I heard you!")
    }

    /// Send a private message to the sender no matter where they wrote from.
    #[command("secret")]
    async fn secret(&self, ctx: Context) -> Result {
        ctx.whisper("This is just between us.")
    }

    /// Post a reminder to #rust every hour on the hour, on weekdays between
    /// 8 a.m. and 4 p.m. Eastern time.  The cron expression uses the 6-field
    /// Quartz format: sec min hour day-of-month month day-of-week.
    #[on(
        cron = "0 0 8-16 * * MON-FRI",
        tz = "America/New_York",
        target = "#rust"
    )]
    async fn hourly_reminder(&self, ctx: Context) -> Result {
        ctx.say("Reminder: be excellent to each other!")
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // In a real scenario you'd point this at an actual IRC server.
    // Attempting to connect to 127.0.0.1:6667 will fail unless a server is running,
    // so we just demonstrate the API compiles correctly.
    println!("basic_bot example compiled successfully.");
    println!("To connect for real, uncomment the lines below and point at a live server:");
    println!("  let bot = MyBot::new(\"ircbot\", \"irc.libera.chat:6667\", [\"#rust\"]).await?;");
    println!("  bot.main_loop().await?;");
    Ok(())
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
//
// Each handler method can be tested directly without a live IRC connection by:
//
//   1. Creating the bot with `MyBot::default()` (no connection is made).
//   2. Building a fake context with `ircbot::testing::TestContext`.
//   3. Calling the handler method and asserting on the captured replies.
//
// `TestContext::channel` simulates a PRIVMSG sent to a channel.
// `TestContext::private` simulates a direct private message to the bot.
// `TestContext::builder()` gives full control over every field.

#[cfg(test)]
mod tests {
    use super::*;
    use ircbot::testing::TestContext;

    // ── !ping ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn ping_replies_with_nick_prefix_in_channel() {
        let bot = MyBot::default();
        let mut tc = TestContext::channel("#test", "alice", "!ping");
        bot.ping(tc.take_ctx()).await.unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #test :alice, pong!\r\n".to_string()),
        );
    }

    #[tokio::test]
    async fn ping_replies_without_prefix_in_query() {
        let bot = MyBot::default();
        let mut tc = TestContext::private("alice", "!ping");
        bot.ping(tc.take_ctx()).await.unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG alice :pong!\r\n".to_string()),
        );
    }

    // ── !echo ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn echo_says_the_provided_text() {
        let bot = MyBot::default();
        let mut tc = TestContext::channel("#test", "alice", "!echo hello world");
        bot.echo(tc.take_ctx(), "hello world".to_string())
            .await
            .unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #test :hello world\r\n".to_string()),
        );
    }

    // ── hello * ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn greet_says_hello_with_captured_name() {
        let bot = MyBot::default();
        let mut tc = TestContext::channel("#test", "alice", "hello rust");
        bot.greet(tc.take_ctx(), "rust".to_string()).await.unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #test :Hello, rust!\r\n".to_string()),
        );
    }

    // ── JOIN #rust ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn welcome_greets_new_user_in_rust_channel() {
        let bot = MyBot::default();
        // Simulate a context where the sender joins #rust.
        let mut tc = TestContext::builder()
            .target("#rust")
            .is_channel(true)
            .sender_nick("newuser")
            .build();
        bot.welcome(tc.take_ctx()).await.unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #rust :Welcome to #rust, newuser!\r\n".to_string()),
        );
    }

    // ── mention ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn on_mention_echoes_the_addressed_text() {
        let bot = MyBot::default();
        let mut tc = TestContext::channel("#test", "alice", "testbot: greetings");
        // The framework extracts the text after "testbot: " as a capture;
        // in a direct call we pass it ourselves.
        bot.on_mention(tc.take_ctx(), "greetings".to_string())
            .await
            .unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #test :alice, You said: greetings\r\n".to_string()),
        );
    }

    // ── mention target = "#rust" ──────────────────────────────────────────────

    #[tokio::test]
    async fn on_mention_rust_sends_notice() {
        let bot = MyBot::default();
        let mut tc = TestContext::builder()
            .target("#rust")
            .is_channel(true)
            .sender_nick("alice")
            .build();
        bot.on_mention_rust(tc.take_ctx()).await.unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("NOTICE #rust :I heard you!\r\n".to_string()),
        );
    }

    // ── !secret ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn secret_whispers_to_sender_regardless_of_channel() {
        let bot = MyBot::default();
        // Even though the message arrives in a channel, whisper goes to the
        // sender's nick directly.
        let mut tc = TestContext::channel("#test", "alice", "!secret");
        bot.secret(tc.take_ctx()).await.unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG alice :This is just between us.\r\n".to_string()),
        );
    }

    // ── cron handler ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn hourly_reminder_says_to_channel() {
        let bot = MyBot::default();
        // Cron handlers can be called directly like any other handler.
        let mut tc = TestContext::builder()
            .target("#rust")
            .is_channel(true)
            .build();
        bot.hourly_reminder(tc.take_ctx()).await.unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #rust :Reminder: be excellent to each other!\r\n".to_string()),
        );
    }
}
