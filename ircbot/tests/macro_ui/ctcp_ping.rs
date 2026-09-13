use ircbot::{bot, Context, Result};

#[bot]
impl CtcpPingBot {
    #[on(ctcp = "PING")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.say("pong")
    }
}

fn main() {}
