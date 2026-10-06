use ircbot::{plugin, Context, Result};

#[plugin]
impl NoName {
    #[command("hi")]
    async fn hi(&self, ctx: Context) -> Result {
        ctx.say("hi")
    }
}

fn main() {}
