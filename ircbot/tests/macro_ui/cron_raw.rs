use ircbot::{bot, Context, Result};

#[bot]
impl CronBot {
    #[on(cron = "0 0 * * * *", raw)]
    async fn tick(&self, ctx: Context) -> Result {
        ctx.say("tick")
    }
}

fn main() {}
