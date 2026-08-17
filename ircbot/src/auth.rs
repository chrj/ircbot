//! Credentials presented to the server while registering.
//!
//! Everything here is configured through [`Server`](crate::Server) — see
//! [`Server::with_sasl_plain`](crate::Server::with_sasl_plain),
//! [`Server::with_sasl_external`](crate::Server::with_sasl_external), and
//! [`Server::with_password`](crate::Server::with_password). Credentials belong
//! to the server rather than to the running bot because they are needed during
//! the handshake, before a [`State`](crate::State) exists to configure.
//!
//! The exchange itself lives in [`crate::connection`].

use std::fmt;

/// A SASL mechanism, together with the credentials it needs.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Sasl {
    /// `PLAIN`: a username and password sent over the connection. Only use it
    /// on a TLS connection — the credentials are otherwise readable on the
    /// wire.
    Plain {
        /// The account name to authenticate as (`authcid`).
        user: String,
        /// The account password.
        password: String,
    },
    /// `EXTERNAL`: the server derives the account from the TLS client
    /// certificate already presented during the handshake (CertFP), so no
    /// credentials are sent here.
    External,
}

impl Sasl {
    /// The mechanism name as it appears in `AUTHENTICATE` and in the `sasl`
    /// capability value.
    pub(crate) fn mechanism(&self) -> &'static str {
        match self {
            Sasl::Plain { .. } => "PLAIN",
            Sasl::External => "EXTERNAL",
        }
    }

    /// The base64 payload answering the server's `AUTHENTICATE +` challenge.
    ///
    /// `PLAIN` sends `authzid \0 authcid \0 passwd` with an empty `authzid`,
    /// per RFC 4616. `EXTERNAL` sends an empty `authzid`, which base64-encodes
    /// to the empty string; the caller turns that into the `+` the protocol
    /// uses for "no data".
    pub(crate) fn response(&self) -> String {
        match self {
            Sasl::Plain { user, password } => {
                base64_encode(format!("\0{user}\0{password}").as_bytes())
            }
            Sasl::External => String::new(),
        }
    }
}

/// Redacts the password. `Sasl` is reachable from `Server`, which is exactly
/// the sort of value that ends up in a `tracing` field or a `{:?}` of
/// application config.
impl fmt::Debug for Sasl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Sasl::Plain { user, .. } => f
                .debug_struct("Plain")
                .field("user", user)
                .field("password", &"<redacted>")
                .finish(),
            Sasl::External => f.write_str("External"),
        }
    }
}

/// How the bot authenticates to a server, and which IRCv3 capabilities it asks
/// for on the way.
///
/// The default is no authentication and no capabilities, which makes the
/// handshake a bare `NICK`/`USER` exchange.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Auth {
    /// Server password, sent as `PASS` before `NICK`. Distinct from SASL: it
    /// authenticates to the *server*, not to its services.
    pub(crate) password: Option<String>,
    /// SASL mechanism and credentials, or `None` to skip SASL.
    pub(crate) sasl: Option<Sasl>,
    /// Extra IRCv3 capabilities to request alongside `sasl`. Ones the server
    /// does not advertise are skipped.
    pub(crate) extra_caps: Vec<String>,
}

impl Auth {
    /// The capabilities to request, in the order they should be asked for.
    ///
    /// Empty when there is nothing to negotiate, which is the signal to skip
    /// `CAP` entirely and leave the handshake as it was before IRCv3.
    pub(crate) fn wanted_caps(&self) -> Vec<&str> {
        let mut caps = Vec::with_capacity(1 + self.extra_caps.len());
        if self.sasl.is_some() {
            caps.push("sasl");
        }
        caps.extend(self.extra_caps.iter().map(String::as_str));
        caps
    }

    /// Whether the handshake needs a `CAP` exchange at all.
    pub(crate) fn negotiates_caps(&self) -> bool {
        !self.wanted_caps().is_empty()
    }
}

/// Redacts the server password; see the `Debug` impl for [`Sasl`].
impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("sasl", &self.sasl)
            .field("extra_caps", &self.extra_caps)
            .finish()
    }
}

/// The standard base64 alphabet (RFC 4648 §4).
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode `input` as base64 with padding, per RFC 4648 §4.
///
/// Hand-rolled rather than pulled in as a dependency: SASL needs a few dozen
/// bytes encoded once per connection, and the crate keeps its dependency
/// surface small.
fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let group = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        // Every group yields two characters; the third and fourth become `=`
        // padding when the chunk was short.
        out.push(char::from(BASE64_ALPHABET[(group >> 18) as usize & 0x3f]));
        out.push(char::from(BASE64_ALPHABET[(group >> 12) as usize & 0x3f]));
        out.push(if chunk.len() > 1 {
            char::from(BASE64_ALPHABET[(group >> 6) as usize & 0x3f])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(BASE64_ALPHABET[group as usize & 0x3f])
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── base64 ────────────────────────────────────────────────────────────────

    /// The test vectors from RFC 4648 §10, which pin down both the alphabet and
    /// the padding rules.
    #[test]
    fn base64_encodes_the_rfc_4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                base64_encode(input.as_bytes()),
                expected,
                "input: {input:?}"
            );
        }
    }

    /// Bytes outside ASCII must survive, since a password may hold any UTF-8.
    #[test]
    fn base64_encodes_high_bytes() {
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
    }

    /// The NUL separators SASL PLAIN relies on must be encoded, not dropped.
    #[test]
    fn base64_encodes_nul_bytes() {
        assert_eq!(base64_encode(b"\0a\0b"), "AGEAYg==");
    }

    // ── SASL ──────────────────────────────────────────────────────────────────

    /// `PLAIN` is `authzid \0 authcid \0 passwd` with an empty authzid, so
    /// `bot` / `hunter2` must encode to this exact string (RFC 4616).
    #[test]
    fn plain_response_encodes_authzid_authcid_password() {
        let sasl = Sasl::Plain {
            user: "bot".to_string(),
            password: "hunter2".to_string(),
        };
        assert_eq!(sasl.response(), "AGJvdABodW50ZXIy");
        assert_eq!(sasl.mechanism(), "PLAIN");
    }

    #[test]
    fn external_response_is_empty() {
        assert_eq!(Sasl::External.response(), "");
        assert_eq!(Sasl::External.mechanism(), "EXTERNAL");
    }

    // ── redaction ─────────────────────────────────────────────────────────────

    /// A password must never reach a log line or a `{:?}` dump.
    #[test]
    fn debug_redacts_the_sasl_password() {
        let sasl = Sasl::Plain {
            user: "bot".to_string(),
            password: "hunter2".to_string(),
        };
        let rendered = format!("{sasl:?}");
        assert!(!rendered.contains("hunter2"), "password leaked: {rendered}");
        assert!(rendered.contains("bot"), "{rendered}");
    }

    #[test]
    fn debug_redacts_the_server_password() {
        let auth = Auth {
            password: Some("s3cret".to_string()),
            ..Auth::default()
        };
        let rendered = format!("{auth:?}");
        assert!(!rendered.contains("s3cret"), "password leaked: {rendered}");
    }

    // ── capability selection ──────────────────────────────────────────────────

    #[test]
    fn no_credentials_means_no_cap_exchange() {
        assert!(!Auth::default().negotiates_caps());
        assert!(Auth::default().wanted_caps().is_empty());
    }

    /// A server password alone is sent as `PASS`; it needs no capability, so it
    /// must not drag the connection into a `CAP` exchange.
    #[test]
    fn a_server_password_alone_needs_no_cap_exchange() {
        let auth = Auth {
            password: Some("s3cret".to_string()),
            ..Auth::default()
        };
        assert!(!auth.negotiates_caps());
    }

    #[test]
    fn sasl_requests_the_sasl_capability_first() {
        let auth = Auth {
            sasl: Some(Sasl::External),
            extra_caps: vec!["server-time".to_string()],
            ..Auth::default()
        };
        assert_eq!(auth.wanted_caps(), vec!["sasl", "server-time"]);
    }

    #[test]
    fn extra_capabilities_alone_still_negotiate() {
        let auth = Auth {
            extra_caps: vec!["server-time".to_string()],
            ..Auth::default()
        };
        assert!(auth.negotiates_caps());
        assert_eq!(auth.wanted_caps(), vec!["server-time"]);
    }
}
