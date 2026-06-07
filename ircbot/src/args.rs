//! Whitespace tokeniser for command arguments.
//!
//! Used by the code that `#[command]` generates to pull typed positional
//! arguments out of the text following a command. It is exposed through
//! [`crate::internal`] for the macro's benefit; application code rarely needs it
//! directly.

/// A cursor over the whitespace-separated tokens of a command's argument tail.
///
/// Created from the text that follows a command keyword (e.g. the `"3 4"` in
/// `!add 3 4`). [`next_token`](Args::next_token) consumes one token at a time;
/// [`rest`](Args::rest) and [`rest_tokens`](Args::rest_tokens) consume whatever
/// remains. Leading and trailing whitespace is ignored throughout.
pub struct Args<'a> {
    rest: &'a str,
}

impl<'a> Args<'a> {
    /// Create a tokeniser over `tail`, ignoring any leading whitespace.
    #[must_use]
    pub fn new(tail: &'a str) -> Self {
        Args {
            rest: tail.trim_start(),
        }
    }

    /// Consume and return the next whitespace-delimited token, or `None` when
    /// the input is exhausted.
    #[must_use]
    pub fn next_token(&mut self) -> Option<&'a str> {
        self.rest = self.rest.trim_start();
        if self.rest.is_empty() {
            return None;
        }
        let end = self
            .rest
            .find(char::is_whitespace)
            .unwrap_or(self.rest.len());
        let (token, rest) = self.rest.split_at(end);
        self.rest = rest;
        Some(token)
    }

    /// Consume the remaining input and return it as a single trimmed string.
    ///
    /// This is how a trailing `String` argument captures "the rest of the line";
    /// the result may be empty if nothing remains.
    #[must_use]
    pub fn rest(self) -> &'a str {
        self.rest.trim()
    }

    /// Consume the remaining input and return it as a vector of tokens.
    ///
    /// This is how a trailing `Vec<_>` argument captures every remaining word;
    /// the result is empty if nothing remains.
    #[must_use]
    pub fn rest_tokens(self) -> Vec<String> {
        self.rest.split_whitespace().map(str::to_string).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Args;

    #[test]
    fn next_token_yields_words_in_order() {
        let mut args = Args::new("alpha beta gamma");
        assert_eq!(args.next_token(), Some("alpha"));
        assert_eq!(args.next_token(), Some("beta"));
        assert_eq!(args.next_token(), Some("gamma"));
        assert_eq!(args.next_token(), None);
    }

    #[test]
    fn next_token_collapses_runs_of_whitespace() {
        let mut args = Args::new("  alpha \t beta   ");
        assert_eq!(args.next_token(), Some("alpha"));
        assert_eq!(args.next_token(), Some("beta"));
        assert_eq!(args.next_token(), None);
    }

    #[test]
    fn empty_input_yields_no_tokens() {
        let mut args = Args::new("   ");
        assert_eq!(args.next_token(), None);
    }

    #[test]
    fn rest_returns_remaining_text_verbatim_but_trimmed() {
        let mut args = Args::new("set topic  hello   world  ");
        assert_eq!(args.next_token(), Some("set"));
        assert_eq!(args.rest(), "topic  hello   world");
    }

    #[test]
    fn rest_is_empty_when_input_exhausted() {
        let mut args = Args::new("only");
        assert_eq!(args.next_token(), Some("only"));
        assert_eq!(args.rest(), "");
    }

    #[test]
    fn rest_tokens_collects_remaining_words() {
        let mut args = Args::new("a b c d");
        assert_eq!(args.next_token(), Some("a"));
        assert_eq!(args.rest_tokens(), vec!["b", "c", "d"]);
    }
}
