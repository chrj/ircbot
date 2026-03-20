use rustbot2::{bot, Context, Result};

#[bot]
impl MyBot {
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.reply("pong!").await
    }

    #[command("echo")]
    async fn echo(&self, ctx: Context, text: String) -> Result {
        ctx.say(text).await
    }

    #[on(message = "hello *")]
    async fn greet(&self, ctx: Context, who: String) -> Result {
        ctx.say(format!("Hello, {}!", who)).await
    }

    #[on(event = "JOIN", target = "#rust")]
    async fn welcome(&self, ctx: Context) -> Result {
        if let Some(user) = &ctx.sender {
            ctx.say(format!("Welcome to #rust, {}!", user.nick)).await
        } else {
            Ok(())
        }
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // In a real scenario you'd point this at an actual IRC server.
    // Attempting to connect to 127.0.0.1:6667 will fail unless a server is running,
    // so we just demonstrate the API compiles correctly.
    println!("basic_bot example compiled successfully.");
    println!("To connect for real, uncomment the lines below and point at a live server:");
    println!("  let bot = MyBot::new(\"rustbot2\", \"irc.libera.chat:6667\", [\"#rust\"]).await?;");
    println!("  bot.main_loop().await?;");
    Ok(())
}
