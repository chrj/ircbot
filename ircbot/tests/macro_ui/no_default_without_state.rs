use ircbot::{bot, Context, Result};

#[bot(no_default)]
impl PlainBot {
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.say("pong")
    }
}

fn main() {}
