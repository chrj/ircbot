use ircbot::{bot, Context, Result};

#[bot]
impl BadCtcpBot {
    #[on(ctcp = "ACTION waves")]
    async fn waves(&self, ctx: Context) -> Result {
        ctx.say("waves")
    }
}

fn main() {}
