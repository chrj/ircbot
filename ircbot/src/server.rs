//! How to reach an IRC server: an address plus the transport to use.
//!
//! [`Server`] is the value passed to [`State::connect`](crate::State::connect)
//! and to the `#[bot]`-generated `connect` constructor. A bare `"host:port"`
//! string converts into a plaintext `Server`, so the common case stays terse:
//!
//! ```rust,no_run
//! # use ircbot::Server;
//! let plain: Server = "irc.example.net:6667".into();
//! let same = Server::plain("irc.example.net:6667");
//! ```
//!
//! With the `tls` feature enabled, `Server::tls` returns a `TlsServer` that
//! carries the TLS-only options. Because those options live on a separate type
//! they cannot be set on a plaintext server — the mistake is a compile error
//! rather than a silently-ignored setting:
//!
//! ```rust,ignore
//! Server::tls("irc.libera.chat:6697").with_sni("irc.libera.chat")
//! ```
//!
//! # Choosing a port
//!
//! The transport is never inferred from the port number. `6697` is the
//! conventional TLS port and `6667` the conventional plaintext one, but a
//! `Server::plain("…:6697")` connects in plaintext exactly as written. Making
//! the transport explicit means a misconfigured port can never silently
//! downgrade a connection.

use std::fmt;

/// How to reach an IRC server.
///
/// Build one with [`Server::plain`] or (with the `tls` feature)
/// `Server::tls`. A `&str`, `String`, or `&String` holding a `"host:port"`
/// address converts into a plaintext `Server` via [`From`], so anywhere a
/// `Server` is accepted a bare address string works too.
#[derive(Clone)]
pub struct Server {
    /// The `host:port` address to connect to, also used for reconnects.
    pub(crate) addr: String,
    /// TLS configuration, or `None` for a plaintext connection. Without the
    /// `tls` feature [`TlsSettings`] is uninhabited, so this is always `None`.
    pub(crate) tls: Option<TlsSettings>,
}

impl Server {
    /// Connect in plaintext, with no encryption.
    ///
    /// `addr` is a `host:port` pair such as `"irc.example.net:6667"`.
    #[must_use]
    pub fn plain(addr: impl Into<String>) -> Self {
        Server {
            addr: addr.into(),
            tls: None,
        }
    }

    /// Connect over TLS, verifying the server's certificate against the
    /// platform's native root store.
    ///
    /// `addr` is a `host:port` pair such as `"irc.libera.chat:6697"`. The host
    /// part is used for SNI and certificate hostname verification; override it
    /// with [`TlsServer::with_sni`] when connecting by IP address.
    ///
    /// Returns a [`TlsServer`] carrying the TLS-only options; it converts into
    /// a `Server` implicitly wherever one is expected.
    #[cfg(feature = "tls")]
    #[must_use]
    pub fn tls(addr: impl Into<String>) -> TlsServer {
        TlsServer {
            addr: addr.into(),
            settings: TlsSettings::default(),
        }
    }

    /// The `host:port` address this `Server` connects to.
    #[must_use]
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Whether this `Server` uses TLS.
    #[must_use]
    pub fn is_tls(&self) -> bool {
        self.tls.is_some()
    }

    /// The host portion of [`addr`](Server::addr), with any IPv6 brackets
    /// stripped.
    ///
    /// Splits on the **last** colon so that a bracketed IPv6 literal such as
    /// `"[::1]:6697"` yields `"::1"` rather than `"["`. This is the name used
    /// for SNI and certificate verification on a TLS connection, unless
    /// overridden with `TlsServer::with_sni`.
    #[must_use]
    pub fn host(&self) -> &str {
        let host = match self.addr.rsplit_once(':') {
            Some((host, _port)) => host,
            None => &self.addr,
        };
        host.strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host)
    }
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("addr", &self.addr)
            .field("tls", &self.tls)
            .finish()
    }
}

impl fmt::Display for Server {
    /// Renders as `ircs://host:port` for TLS and `irc://host:port` otherwise,
    /// so log lines make the transport obvious.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.is_tls() { "ircs" } else { "irc" };
        write!(f, "{scheme}://{}", self.addr)
    }
}

impl From<&str> for Server {
    fn from(addr: &str) -> Self {
        Server::plain(addr)
    }
}

impl From<String> for Server {
    fn from(addr: String) -> Self {
        Server::plain(addr)
    }
}

impl From<&String> for Server {
    fn from(addr: &String) -> Self {
        Server::plain(addr.clone())
    }
}

// ─── TLS ─────────────────────────────────────────────────────────────────────

/// A TLS server address under construction, returned by [`Server::tls`].
///
/// Carries the options that only make sense for a TLS connection. Convert it
/// into a [`Server`] with [`From`] — which happens automatically wherever an
/// `impl Into<Server>` is accepted, so the builder chain can be passed directly
/// to `connect`.
#[cfg(feature = "tls")]
#[derive(Clone, Debug)]
pub struct TlsServer {
    addr: String,
    settings: TlsSettings,
}

#[cfg(feature = "tls")]
impl TlsServer {
    /// Override the hostname used for SNI and certificate verification.
    ///
    /// By default the host part of the address is used. Set this when
    /// connecting to an IP address whose certificate names a hostname, or when
    /// going through a tunnel that changes the address.
    #[must_use]
    pub fn with_sni(mut self, hostname: impl Into<String>) -> Self {
        self.settings.sni = Some(hostname.into());
        self
    }

    /// Trust an additional root certificate, in PEM form, alongside the
    /// platform's native roots.
    ///
    /// This is the supported way to connect to a network using a private CA or
    /// a self-signed certificate: trust exactly that certificate rather than
    /// disabling verification wholesale. May be called repeatedly; every
    /// certificate found in each PEM blob is added.
    ///
    /// The PEM is parsed when the connection is made, so a malformed blob
    /// surfaces as a connect error rather than a panic here.
    #[must_use]
    pub fn with_extra_root_pem(mut self, pem: impl Into<Vec<u8>>) -> Self {
        self.settings.extra_roots_pem.push(pem.into());
        self
    }

    /// Present a client certificate, in PEM form, for CertFP / SASL EXTERNAL
    /// authentication.
    ///
    /// `pem` must contain the private key and the certificate chain
    /// concatenated — the single-file form produced by, for example:
    ///
    /// ```sh
    /// openssl req -x509 -newkey rsa:4096 -sha256 -days 3650 \
    ///     -nodes -keyout bot.pem -out bot.pem -subj "/CN=mybot"
    /// ```
    ///
    /// The bytes are parsed when the connection is made, so a malformed or
    /// key-less blob surfaces as a connect error rather than a panic here.
    ///
    /// Calling this more than once replaces the previous certificate.
    #[must_use]
    pub fn with_client_cert_pem(mut self, pem: impl Into<Vec<u8>>) -> Self {
        self.settings.client_cert_pem = Some(pem.into());
        self
    }

    /// **Dangerous.** Accept any server certificate without verifying it.
    ///
    /// This disables certificate and hostname verification entirely, which
    /// removes the protection TLS provides against an active
    /// machine-in-the-middle: traffic is still encrypted, but you have no
    /// assurance about who it is encrypted *to*. Passwords sent to the server —
    /// `PASS`, `NickServ IDENTIFY`, SASL — become interceptable.
    ///
    /// It exists for talking to a development server on `localhost` and for
    /// tests. For a real network using a self-signed or private-CA
    /// certificate, use [`with_extra_root_pem`](TlsServer::with_extra_root_pem)
    /// instead: it keeps verification on and trusts exactly the one certificate
    /// you intend to trust.
    ///
    /// Enabling this logs a warning on every connection.
    #[must_use]
    pub fn danger_accept_invalid_certs(mut self) -> Self {
        self.settings.accept_invalid_certs = true;
        self
    }
}

#[cfg(feature = "tls")]
impl From<TlsServer> for Server {
    fn from(tls: TlsServer) -> Self {
        Server {
            addr: tls.addr,
            tls: Some(tls.settings),
        }
    }
}

/// TLS options for a [`Server`].
///
/// Without the `tls` feature this type is uninhabited, so `Option<TlsSettings>`
/// can only ever be `None` and every TLS branch is statically dead.
#[cfg(not(feature = "tls"))]
#[derive(Clone, Debug)]
pub(crate) enum TlsSettings {}

/// TLS options for a [`Server`].
///
/// PEM inputs are stored unparsed and turned into a rustls configuration when
/// the connection is made, so this type stays cheap to clone (the reconnect
/// path clones it) and builder methods stay infallible.
#[cfg(feature = "tls")]
#[derive(Clone, Default)]
pub(crate) struct TlsSettings {
    /// Hostname for SNI and certificate verification; `None` means "use the
    /// host part of the address".
    sni: Option<String>,
    /// Extra trust roots in PEM form, added to the platform's native roots.
    extra_roots_pem: Vec<Vec<u8>>,
    /// Client certificate chain plus private key, in PEM form.
    client_cert_pem: Option<Vec<u8>>,
    /// Skip certificate verification entirely. See
    /// [`TlsServer::danger_accept_invalid_certs`].
    accept_invalid_certs: bool,
}

/// Redacts the client certificate blob, which contains a private key. Deriving
/// `Debug` would print it, and `Server` is exactly the sort of value that ends
/// up in a `tracing` field or a `{:?}` of application config.
#[cfg(feature = "tls")]
impl fmt::Debug for TlsSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsSettings")
            .field("sni", &self.sni)
            .field("extra_roots", &self.extra_roots_pem.len())
            .field(
                "client_cert",
                &self.client_cert_pem.as_ref().map(|_| "<redacted>"),
            )
            .field("accept_invalid_certs", &self.accept_invalid_certs)
            .finish()
    }
}

#[cfg(feature = "tls")]
mod connector {
    use std::sync::Arc;

    use tokio_rustls::rustls::client::danger::{
        HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
    };
    use tokio_rustls::rustls::crypto::{
        ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider,
    };
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
    use tokio_rustls::rustls::{
        ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
    };
    use tokio_rustls::TlsConnector;

    use super::TlsSettings;
    use crate::BoxError;

    impl TlsSettings {
        /// Resolve the SNI / certificate-verification hostname for `host` (the
        /// host part of the server address), honouring any
        /// [`with_sni`](super::TlsServer::with_sni) override.
        ///
        /// # Errors
        ///
        /// Returns an error if the name is neither a valid DNS name nor an IP
        /// address.
        pub(crate) fn server_name(&self, host: &str) -> Result<ServerName<'static>, BoxError> {
            let name = self.sni.as_deref().unwrap_or(host);
            ServerName::try_from(name)
                .map(|name| name.to_owned())
                .map_err(|e| {
                    format!(
                        "invalid TLS server name {name:?}: {e}. It must be a DNS hostname or an \
                         IP address; when connecting to an IP whose certificate names a hostname, \
                         set the expected name with Server::tls(..).with_sni(\"host.example.net\")"
                    )
                    .into()
                })
        }

        /// Whether certificate verification is disabled.
        pub(crate) fn accepts_invalid_certs(&self) -> bool {
            self.accept_invalid_certs
        }

        /// Build a rustls connector from these settings.
        ///
        /// The crypto provider is passed explicitly rather than relying on
        /// rustls' process-global default: installing that default is the
        /// application's prerogative, and a library that does it behind the
        /// caller's back can break an application that wanted a different one.
        ///
        /// # Errors
        ///
        /// Returns an error if the platform root store cannot be read, if any
        /// supplied PEM is malformed, or if the client certificate and key are
        /// not a usable pair.
        pub(crate) fn connector(&self) -> Result<TlsConnector, BoxError> {
            let provider = Arc::new(ring::default_provider());

            let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
                .with_safe_default_protocol_versions()
                .map_err(|e| format!("failed to configure TLS protocol versions: {e}"))?;

            let builder = if self.accept_invalid_certs {
                builder
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(NoVerification(provider)))
            } else {
                builder.with_root_certificates(self.roots()?)
            };

            let config = match &self.client_cert_pem {
                Some(pem) => {
                    let (chain, key) = Self::parse_client_cert(pem)?;
                    builder.with_client_auth_cert(chain, key).map_err(|e| {
                        format!(
                            "client certificate rejected: {e}. Check that the PEM passed to \
                             with_client_cert_pem contains a private key matching its certificate"
                        )
                    })?
                }
                None => builder.with_no_client_auth(),
            };

            Ok(TlsConnector::from(Arc::new(config)))
        }

        /// Assemble the trust store: the platform's native roots plus any
        /// caller-supplied PEM roots.
        fn roots(&self) -> Result<RootCertStore, BoxError> {
            let mut roots = RootCertStore::empty();

            let native = rustls_native_certs::load_native_certs();
            // Individual unreadable files are common and harmless (a symlink
            // into a removed package, a permission-denied entry). Only a store
            // that yielded *nothing* is fatal, since that means no verification
            // could ever succeed.
            for error in &native.errors {
                tracing::debug!(%error, "skipping unreadable native root certificate");
            }
            if native.certs.is_empty() {
                let errors = native
                    .errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(format!(
                    "no usable certificates in the platform root store ({errors}). Install your \
                     distribution's CA bundle (e.g. the ca-certificates package), or trust a \
                     specific certificate with Server::tls(..).with_extra_root_pem(..)"
                )
                .into());
            }
            let native_count = native.certs.len();
            roots.add_parsable_certificates(native.certs);

            for (i, pem) in self.extra_roots_pem.iter().enumerate() {
                let certs = CertificateDer::pem_slice_iter(pem)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| {
                        format!(
                            "failed to parse the PEM passed to with_extra_root_pem (#{i}): {e}. \
                             It must contain one or more -----BEGIN CERTIFICATE----- blocks"
                        )
                    })?;
                if certs.is_empty() {
                    return Err(format!(
                        "the PEM passed to with_extra_root_pem (#{i}) contains no certificates. \
                         It must contain at least one -----BEGIN CERTIFICATE----- block"
                    )
                    .into());
                }
                for cert in certs {
                    roots.add(cert).map_err(|e| {
                        format!("failed to trust the certificate passed to with_extra_root_pem (#{i}): {e}")
                    })?;
                }
            }

            tracing::debug!(
                native = native_count,
                extra = self.extra_roots_pem.len(),
                total = roots.len(),
                "built TLS trust store"
            );

            Ok(roots)
        }

        /// Split a combined key+chain PEM into a certificate chain and a
        /// private key.
        fn parse_client_cert(
            pem: &[u8],
        ) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), BoxError> {
            let chain = CertificateDer::pem_slice_iter(pem)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    format!(
                        "failed to parse the certificate in the PEM passed to \
                         with_client_cert_pem: {e}"
                    )
                })?;
            if chain.is_empty() {
                return Err(
                    "the PEM passed to with_client_cert_pem contains no certificate. It \
                            must hold both the private key and the certificate chain, \
                            concatenated"
                        .into(),
                );
            }

            let key = PrivateKeyDer::from_pem_slice(pem).map_err(|e| {
                format!(
                    "failed to parse the private key in the PEM passed to with_client_cert_pem: \
                     {e}. It must hold both the private key and the certificate chain, \
                     concatenated"
                )
            })?;

            Ok((chain, key))
        }
    }

    /// A [`ServerCertVerifier`] that accepts every certificate.
    ///
    /// Installed only by
    /// [`danger_accept_invalid_certs`](super::TlsServer::danger_accept_invalid_certs).
    /// Signature verification is still performed against the presented key —
    /// only the question of whether that key belongs to anyone trustworthy is
    /// skipped.
    #[derive(Debug)]
    struct NoVerification(Arc<CryptoProvider>);

    impl ServerCertVerifier for NoVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
            verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
            verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── construction ───────────────────────────────────────────────────────────

    #[test]
    fn plain_is_not_tls() {
        let server = Server::plain("irc.example.net:6667");
        assert_eq!(server.addr(), "irc.example.net:6667");
        assert!(!server.is_tls());
    }

    #[test]
    fn str_converts_to_a_plaintext_server() {
        let server: Server = "irc.example.net:6667".into();
        assert!(!server.is_tls());
        assert_eq!(server.addr(), "irc.example.net:6667");
    }

    #[test]
    fn string_and_string_ref_convert_to_a_plaintext_server() {
        let owned = String::from("irc.example.net:6667");
        assert_eq!(Server::from(&owned).addr(), "irc.example.net:6667");
        assert_eq!(Server::from(owned).addr(), "irc.example.net:6667");
    }

    /// A TLS-conventional port on a `plain` server stays plaintext: the
    /// transport is never inferred from the port number.
    #[test]
    fn port_does_not_imply_tls() {
        assert!(!Server::plain("irc.libera.chat:6697").is_tls());
    }

    // ── host extraction ────────────────────────────────────────────────────────

    #[test]
    fn host_strips_the_port() {
        assert_eq!(
            Server::plain("irc.example.net:6667").host(),
            "irc.example.net"
        );
    }

    #[test]
    fn host_unwraps_bracketed_ipv6() {
        assert_eq!(Server::plain("[2001:db8::1]:6697").host(), "2001:db8::1");
    }

    #[test]
    fn host_handles_an_address_without_a_port() {
        assert_eq!(Server::plain("irc.example.net").host(), "irc.example.net");
    }

    // ── display ────────────────────────────────────────────────────────────────

    #[test]
    fn display_shows_the_irc_scheme_for_plaintext() {
        assert_eq!(
            Server::plain("irc.example.net:6667").to_string(),
            "irc://irc.example.net:6667"
        );
    }

    // ── TLS ────────────────────────────────────────────────────────────────────

    #[cfg(feature = "tls")]
    mod tls {
        use super::*;

        #[test]
        fn tls_server_converts_into_a_tls_server_value() {
            let server: Server = Server::tls("irc.libera.chat:6697").into();
            assert!(server.is_tls());
            assert_eq!(server.addr(), "irc.libera.chat:6697");
        }

        #[test]
        fn display_shows_the_ircs_scheme_for_tls() {
            let server: Server = Server::tls("irc.libera.chat:6697").into();
            assert_eq!(server.to_string(), "ircs://irc.libera.chat:6697");
        }

        #[test]
        fn verification_is_on_by_default() {
            let server: Server = Server::tls("irc.libera.chat:6697").into();
            assert!(!server.tls.unwrap().accepts_invalid_certs());
        }

        #[test]
        fn danger_accept_invalid_certs_disables_verification() {
            let server: Server = Server::tls("irc.libera.chat:6697")
                .danger_accept_invalid_certs()
                .into();
            assert!(server.tls.unwrap().accepts_invalid_certs());
        }

        #[test]
        fn server_name_defaults_to_the_address_host() {
            let server: Server = Server::tls("irc.libera.chat:6697").into();
            let name = server
                .tls
                .as_ref()
                .unwrap()
                .server_name(server.host())
                .unwrap();
            assert_eq!(format!("{name:?}"), r#"DnsName("irc.libera.chat")"#);
        }

        #[test]
        fn with_sni_overrides_the_address_host() {
            let server: Server = Server::tls("127.0.0.1:6697")
                .with_sni("irc.example.net")
                .into();
            let name = server
                .tls
                .as_ref()
                .unwrap()
                .server_name(server.host())
                .unwrap();
            assert_eq!(format!("{name:?}"), r#"DnsName("irc.example.net")"#);
        }

        #[test]
        fn an_ip_address_is_a_valid_server_name() {
            let server: Server = Server::tls("127.0.0.1:6697").into();
            assert!(server
                .tls
                .as_ref()
                .unwrap()
                .server_name(server.host())
                .is_ok());
        }

        #[test]
        fn an_unusable_server_name_is_rejected_with_advice() {
            let server: Server = Server::tls("not a hostname:6697").into();
            let err = server
                .tls
                .as_ref()
                .unwrap()
                .server_name(server.host())
                .unwrap_err()
                .to_string();
            assert!(err.contains("with_sni"), "unhelpful error: {err}");
        }

        // ── PEM handling ───────────────────────────────────────────────────────

        /// `TlsConnector` is not `Debug`, so `unwrap_err` is unavailable.
        fn connector_error(server: Server) -> String {
            match server.tls.expect("a TLS server").connector() {
                Ok(_) => panic!("expected the connector to be rejected"),
                Err(e) => e.to_string(),
            }
        }

        #[test]
        fn a_malformed_extra_root_is_rejected_with_advice() {
            let err = connector_error(
                Server::tls("irc.example.net:6697")
                    .with_extra_root_pem(&b"not a certificate"[..])
                    .into(),
            );
            assert!(err.contains("BEGIN CERTIFICATE"), "unhelpful error: {err}");
        }

        #[test]
        fn a_client_cert_without_a_certificate_is_rejected_with_advice() {
            let err = connector_error(
                Server::tls("irc.example.net:6697")
                    .with_client_cert_pem(&b"not a certificate"[..])
                    .into(),
            );
            assert!(err.contains("concatenated"), "unhelpful error: {err}");
        }

        /// The private key must never reach a log line or a `{:?}` dump.
        #[test]
        fn debug_redacts_the_client_certificate() {
            let server: Server = Server::tls("irc.example.net:6697")
                .with_client_cert_pem(&b"-----BEGIN PRIVATE KEY-----secret"[..])
                .into();
            let rendered = format!("{server:?}");
            assert!(!rendered.contains("secret"), "key leaked: {rendered}");
            assert!(rendered.contains("<redacted>"), "{rendered}");
        }
    }
}
