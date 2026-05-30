use ircbot::bot;

// `#[bot]` requires a simple struct name; a path type must be rejected.
#[bot]
impl some_module::SomeBot {}

fn main() {}
