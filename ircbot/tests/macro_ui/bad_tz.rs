use ircbot::{bot, Context, Result};

#[bot]
impl BadTzBot {
    #[on(cron = "0 0 * * * *", tz = "Mars/Phobos")]
    async fn tick(&self, ctx: Context) -> Result {
        ctx.say("tick")
    }
}

fn main() {}
