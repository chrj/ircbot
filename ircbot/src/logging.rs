//! Logging and diagnostics.
//!
//! The framework emits all of its diagnostics through the
//! [`tracing`](https://docs.rs/tracing) facade and installs **no** subscriber of
//! its own — the level, format, and destination are entirely yours to choose.
//! Until you install a subscriber, the events are silently dropped.
//!
//! A minimal setup using
//! [`tracing-subscriber`](https://docs.rs/tracing-subscriber) prints to stderr
//! and reads its filter from the `RUST_LOG` environment variable:
//!
//! ```rust,ignore
//! tracing_subscriber::fmt()
//!     .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
//!     .init();
//! ```
//!
//! Connection lifecycle and handler failures are logged at `info`/`warn`/`error`.
//!
//! # Raw protocol logging
//!
//! Every line read from and written to the server is emitted at the `TRACE`
//! level on the dedicated [`PROTOCOL_LOG_TARGET`] target, tagged with a `dir`
//! field of `"recv"` or `"send"`. This is **opt-in**: it stays silent unless you
//! enable that target at `TRACE`, for example:
//!
//! ```sh
//! RUST_LOG=ircbot=info,ircbot::protocol=trace cargo run
//! ```
//!
//! Protocol traces contain the full, unredacted message content, so enable them
//! only while debugging.

/// Tracing target for raw IRC protocol lines.
///
/// Every line read from or written to the server is emitted at the `TRACE`
/// level on this target, tagged with a `dir` field (`"recv"` or `"send"`).
/// Protocol logging is therefore **opt-in**: it stays silent unless your
/// subscriber enables this target at `TRACE`, e.g. with the environment filter
/// `ircbot::protocol=trace`.
///
/// Note that these lines contain the full, unredacted message content; only
/// enable them when debugging.
pub const PROTOCOL_LOG_TARGET: &str = "ircbot::protocol";
