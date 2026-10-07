//! A bot made of the standard `seen` plugin, with the built-in `!help`.
//!
//! Run with:
//!
//!     cargo run -p ircbot-plugins --example seen_bot

use ircbot::bot;
use ircbot::store::Store;
use ircbot_plugins::Seen;

/// A bot whose commands all come from plugins.
#[bot]
impl SeenBot {}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // As with the examples of `ircbot`, we do not connect here. The in-memory
    // store shows that the plugin opens. A real bot opens a file, so the data
    // stays after a restart.
    let _seen = Seen::open(&Store::memory()?).await?;

    println!("seen_bot example compiled successfully.");
    println!("To connect for real:");
    println!("  let store = Store::open(\"bot.db\")?;");
    println!("  SeenBot::new(\"ircbot\", \"irc.libera.chat:6667\", [\"#rust\"])");
    println!("      .with_help()");
    println!("      .plugin(Seen::open(&store).await?)");
    println!("      .main_loop()");
    println!("      .await?;");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bot with the plugin passes the checks of `main_loop`, and only
    /// then fails to connect, because the server does not exist.
    #[tokio::test]
    async fn the_bot_with_the_plugin_passes_the_checks() {
        let store = Store::memory().unwrap();
        let bot = SeenBot::new("seenbot", "127.0.0.1:1", ["#rust"])
            .with_help()
            .plugin(Seen::open(&store).await.unwrap());

        let err = bot.main_loop().await.expect_err("there is no server");

        assert!(
            matches!(
                err.downcast_ref::<ircbot::StartError>(),
                Some(ircbot::StartError::Connect { .. })
            ),
            "got {err}"
        );
    }
}
