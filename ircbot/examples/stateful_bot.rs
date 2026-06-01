//! Demonstrates a bot that keeps private state via `#[bot(state = Type)]`.
//!
//! The state type must implement `Default` and be `Send + Sync + 'static`.
//! Handlers receive `&self`, so mutable state uses interior mutability — here an
//! `AtomicUsize`. The macro exposes the value as a public `self.state` field.

use std::sync::atomic::{AtomicUsize, Ordering};

use ircbot::{bot, Context, Result};

/// Per-bot state. Wrapped in atomics/locks so handlers (which get `&self`) can
/// mutate it through the shared `Arc`.
#[derive(Default)]
struct Counter {
    pings: AtomicUsize,
}

#[bot(state = Counter)]
impl CounterBot {
    /// Count how many times `!ping` has been seen and report the running total.
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        let n = self.state.pings.fetch_add(1, Ordering::Relaxed) + 1;
        ctx.reply(format!("pong #{n}"))
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // As with `basic_bot`, we don't connect here — just show the API compiles.
    println!("stateful_bot example compiled successfully.");
    println!("To start a non-default state, assign the field after `new()`:");
    println!("  let mut bot = CounterBot::new(\"ircbot\", \"irc.libera.chat:6667\", [\"#rust\"]).await?;");
    println!("  bot.state = Counter::default();");
    println!("  bot.main_loop().await?;");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ircbot::testing::TestContext;

    /// The state field is accessible from a handler and its interior-mutable
    /// counter persists across calls on the same bot instance.
    #[tokio::test]
    async fn ping_increments_persistent_counter() {
        let bot = CounterBot::default();

        let mut tc = TestContext::channel("#test", "alice", "!ping");
        bot.ping(tc.take_ctx()).await.unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #test :alice, pong #1\r\n".to_string()),
        );

        let mut tc2 = TestContext::channel("#test", "alice", "!ping");
        bot.ping(tc2.take_ctx()).await.unwrap();
        assert_eq!(
            tc2.next_reply(),
            Some("PRIVMSG #test :alice, pong #2\r\n".to_string()),
        );
    }
}
