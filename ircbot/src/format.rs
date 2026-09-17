//! Removal of the IRC formatting codes from text.
//!
//! A client can put formatting codes in a message: bold, italic, colour, and
//! more. A bot that logs text, learns from text, or matches text against a
//! pattern usually wants the text without them. [`strip`] removes them and
//! keeps the rest of the text.
//!
//! [`Context::plain_text`](crate::Context::plain_text) applies [`strip`] to the
//! text of the message that fired a handler.
//!
//! # Codes
//!
//! | Code | Meaning |
//! |------|---------|
//! | `\x02` | bold |
//! | `\x03` | colour: 1 or 2 digits for the foreground, then optionally `,` and 1 or 2 digits for the background |
//! | `\x04` | hex colour: 6 hex digits, then optionally `,` and 6 hex digits |
//! | `\x0F` | reset |
//! | `\x11` | monospace |
//! | `\x16` | reverse |
//! | `\x1D` | italic |
//! | `\x1E` | strikethrough |
//! | `\x1F` | underline |
//!
//! # Example
//!
//! ```
//! use ircbot::format::strip;
//!
//! assert_eq!(strip("\x02bold\x02 and \x0304,08colour\x03"), "bold and colour");
//! // A colour takes at most two digits, so the text after it stays.
//! assert_eq!(strip("\x0312345 kroner"), "345 kroner");
//! ```

use std::borrow::Cow;
use std::iter::Peekable;
use std::str::Chars;

/// Bold.
const BOLD: char = '\x02';
/// Colour, with 1 or 2 decimal digits per colour.
const COLOR: char = '\x03';
/// Colour, with 6 hex digits per colour.
const HEX_COLOR: char = '\x04';
/// Reset all formatting.
const RESET: char = '\x0F';
/// Monospace.
const MONOSPACE: char = '\x11';
/// Reverse the foreground and background colours.
const REVERSE: char = '\x16';
/// Italic.
const ITALIC: char = '\x1D';
/// Strikethrough.
const STRIKETHROUGH: char = '\x1E';
/// Underline.
const UNDERLINE: char = '\x1F';

/// How many decimal digits a colour code takes, at most.
const COLOR_DIGITS: usize = 2;
/// How many hex digits one hex colour takes, exactly.
const HEX_COLOR_DIGITS: usize = 6;

/// Remove the IRC formatting codes from `text`.
///
/// A colour code takes at most two digits, so the digits of the text that
/// follow a colour are kept: `"\x0312345 kroner"` is colour 12 and the text
/// `"345 kroner"`. A `\x03` without digits resets the colour and is removed on
/// its own. A `\x04` keeps its digits unless exactly six hex digits follow.
///
/// # Example
///
/// ```
/// use ircbot::format::strip;
///
/// assert_eq!(strip("\x1Ditalic\x1D"), "italic");
/// ```
#[must_use]
pub fn strip(text: &str) -> String {
    strip_cow(text).into_owned()
}

/// Remove the IRC formatting codes from `text`, without a copy when `text`
/// carries no code.
///
/// The dispatch calls this for every message, so the usual text, which has no
/// codes, must cost nothing.
pub(crate) fn strip_cow(text: &str) -> Cow<'_, str> {
    if !has_code(text) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(strip_codes(text))
}

/// Whether `text` carries a formatting code. The codes are ASCII control
/// characters, so a byte scan cannot match part of a character of another
/// script.
fn has_code(text: &str) -> bool {
    text.bytes().any(|b| {
        matches!(
            b as char,
            BOLD | COLOR
                | HEX_COLOR
                | RESET
                | MONOSPACE
                | REVERSE
                | ITALIC
                | STRIKETHROUGH
                | UNDERLINE
        )
    })
}

fn strip_codes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            BOLD | RESET | MONOSPACE | REVERSE | ITALIC | STRIKETHROUGH | UNDERLINE => {}
            COLOR => skip_color(&mut chars),
            HEX_COLOR => skip_hex_color(&mut chars),
            c => out.push(c),
        }
    }
    out
}

/// Consume the digits of a `\x03` colour code: 1 or 2 digits for the
/// foreground, then optionally `,` and 1 or 2 digits for the background.
fn skip_color(chars: &mut Peekable<Chars>) {
    if skip_color_digits(chars) == 0 {
        // A `\x03` on its own resets the colour and takes no digits.
        return;
    }
    // The comma belongs to the code only when a colour follows it. The probe
    // keeps the comma in the text when it does not.
    let mut probe = chars.clone();
    if probe.next() == Some(',') && skip_color_digits(&mut probe) > 0 {
        *chars = probe;
    }
}

/// Consume the digits of a `\x04` hex colour code: 6 hex digits for the
/// foreground, then optionally `,` and 6 hex digits for the background.
fn skip_hex_color(chars: &mut Peekable<Chars>) {
    if !skip_hex_color_digits(chars) {
        // Without six hex digits the code carries no colour, and what follows
        // it is text.
        return;
    }
    let mut probe = chars.clone();
    if probe.next() == Some(',') && skip_hex_color_digits(&mut probe) {
        *chars = probe;
    }
}

/// Consume at most [`COLOR_DIGITS`] decimal digits, and return how many.
fn skip_color_digits(chars: &mut Peekable<Chars>) -> usize {
    let mut taken = 0;
    while taken < COLOR_DIGITS {
        match chars.peek() {
            Some(c) if c.is_ascii_digit() => {
                chars.next();
                taken += 1;
            }
            _ => break,
        }
    }
    taken
}

/// Consume exactly [`HEX_COLOR_DIGITS`] hex digits and return `true`. Consume
/// nothing and return `false` when fewer follow.
fn skip_hex_color_digits(chars: &mut Peekable<Chars>) -> bool {
    let mut probe = chars.clone();
    for _ in 0..HEX_COLOR_DIGITS {
        match probe.next() {
            Some(c) if c.is_ascii_hexdigit() => {}
            _ => return false,
        }
    }
    *chars = probe;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── one code at a time ────────────────────────────────────────────────────

    #[test]
    fn every_code_of_the_table_is_removed() {
        let cases = [
            ("\x02bold\x02", "bold"),
            ("\x0304red\x03", "red"),
            ("\x04FF0000hex\x04", "hex"),
            ("\x0Freset", "reset"),
            ("\x11mono\x11", "mono"),
            ("\x16reverse\x16", "reverse"),
            ("\x1Ditalic\x1D", "italic"),
            ("\x1Estrike\x1E", "strike"),
            ("\x1Funder\x1F", "under"),
        ];
        for (input, want) in cases {
            assert_eq!(strip(input), want, "input: {input:?}");
        }
    }

    #[test]
    fn text_without_codes_is_unchanged() {
        assert_eq!(strip("plain text, 100% of it"), "plain text, 100% of it");
    }

    #[test]
    fn empty_text_stays_empty() {
        assert_eq!(strip(""), "");
    }

    // ── colour ────────────────────────────────────────────────────────────────

    #[test]
    fn colour_with_one_digit_is_removed() {
        assert_eq!(strip("\x034red"), "red");
    }

    #[test]
    fn colour_with_two_digits_is_removed() {
        assert_eq!(strip("\x0312blue"), "blue");
    }

    #[test]
    fn colour_with_a_background_is_removed() {
        assert_eq!(strip("\x0304,08warning"), "warning");
    }

    #[test]
    fn colour_with_one_digit_per_colour_is_removed() {
        assert_eq!(strip("\x034,8warning"), "warning");
    }

    #[test]
    fn digits_after_a_two_digit_colour_stay() {
        assert_eq!(strip("\x0312345 kroner"), "345 kroner");
    }

    #[test]
    fn digits_after_a_background_stay() {
        assert_eq!(strip("\x0304,0815 items"), "15 items");
    }

    #[test]
    fn colour_without_digits_is_removed() {
        assert_eq!(strip("red \x03back to normal"), "red back to normal");
    }

    #[test]
    fn comma_after_a_colour_stays_when_no_digit_follows() {
        assert_eq!(strip("\x0304, and then"), ", and then");
    }

    #[test]
    fn comma_stays_when_the_colour_has_no_digits() {
        assert_eq!(strip("\x03,8 items"), ",8 items");
    }

    // ── hex colour ────────────────────────────────────────────────────────────

    #[test]
    fn hex_colour_is_removed() {
        assert_eq!(strip("\x04FF0000red"), "red");
    }

    #[test]
    fn hex_colour_with_a_background_is_removed() {
        assert_eq!(strip("\x04FF0000,00FF00mixed"), "mixed");
    }

    #[test]
    fn hex_colour_without_six_digits_keeps_its_text() {
        assert_eq!(strip("\x04FF00 red"), "FF00 red");
    }

    #[test]
    fn hex_digits_after_a_hex_colour_stay() {
        assert_eq!(strip("\x04FF0000ABCDEF is a colour"), "ABCDEF is a colour");
    }

    #[test]
    fn comma_after_a_hex_colour_stays_without_a_second_colour() {
        assert_eq!(strip("\x04FF0000, and more"), ", and more");
    }

    // ── combinations ──────────────────────────────────────────────────────────

    #[test]
    fn several_codes_in_one_line_are_removed() {
        assert_eq!(
            strip("\x02bold\x02 and \x0304,08colour\x03 and \x1Ditalic\x0F"),
            "bold and colour and italic",
        );
    }

    #[test]
    fn text_of_other_scripts_is_kept() {
        assert_eq!(strip("\x02Ræv på vej\x02 — 10 km"), "Ræv på vej — 10 km");
    }
}
