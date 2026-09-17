use ircbot::{bot, Context, Result};

#[bot]
impl ScopeBot {
    #[on(mention, scope = "query")]
    async fn answer(&self, ctx: Context) -> Result {
        ctx.say("hi")
    }
}

fn main() {}
