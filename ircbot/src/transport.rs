//! The byte stream underneath the IRC protocol: plain TCP or TLS.
//!
//! [`State`](crate::State) holds the two halves of a connection rather than the
//! stream itself, because the read loop and the write task run concurrently in
//! separate tasks. Both halves are enums rather than boxed trait objects so the
//! plaintext path keeps using `TcpStream`'s lock-free
//! [`into_split`](tokio::net::TcpStream::into_split); only TLS pays for the
//! [`tokio::io::split`] lock, since a `TlsStream` cannot be split any other way.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use crate::{server::Server, BoxError};

/// An established connection to an IRC server, already split for concurrent
/// reading and writing.
pub(crate) struct Connection {
    pub(crate) reader: ReadHalf,
    pub(crate) writer: WriteHalf,
    /// The raw file descriptor, when this connection can survive an `exec`.
    ///
    /// `None` for TLS: the socket would be inherited fine, but the session keys,
    /// record sequence numbers, and any partially-read record live in this
    /// process's memory and die with it, so the successor would inherit a stream
    /// it cannot decrypt. See [`crate::hot_reload`].
    #[cfg(unix)]
    pub(crate) raw_fd: Option<std::os::unix::io::RawFd>,
}

/// Open a connection to `server`, performing the TLS handshake if configured.
///
/// # Errors
///
/// Returns an error if the TCP connection cannot be established, if the TLS
/// configuration is unusable, or if the TLS handshake fails (an untrusted
/// certificate, a hostname mismatch, or a server that is not speaking TLS at
/// all).
pub(crate) async fn connect(server: &Server) -> Result<Connection, BoxError> {
    let stream = TcpStream::connect(server.addr()).await.map_err(|e| {
        format!(
            "failed to connect to {}: {e}. Check the address, the port, and that the server is \
             reachable",
            server.addr()
        )
    })?;

    #[cfg(unix)]
    let raw_fd = {
        use std::os::unix::io::AsRawFd;
        stream.as_raw_fd()
    };

    match &server.tls {
        None => {
            let (read_half, write_half) = stream.into_split();
            Ok(Connection {
                reader: ReadHalf::Plain(read_half),
                writer: WriteHalf::Plain(write_half),
                #[cfg(unix)]
                raw_fd: Some(raw_fd),
            })
        }

        #[cfg(feature = "tls")]
        Some(tls) => {
            if tls.accepts_invalid_certs() {
                tracing::warn!(
                    server = %server,
                    "TLS certificate verification is disabled — the connection is encrypted but \
                     unauthenticated, so it is not protected against interception. Use \
                     with_extra_root_pem to trust a specific certificate instead"
                );
            }

            let server_name = tls.server_name(server.host())?;
            let connector = tls.connector()?;
            let stream = connector.connect(server_name, stream).await.map_err(|e| {
                format!(
                    "TLS handshake with {} failed: {e}. Check that the port speaks TLS directly \
                     (6697 conventionally, not 6667), that the certificate is issued for {:?}, \
                     and that its issuer is trusted — trust a private CA or self-signed \
                     certificate with Server::tls(..).with_extra_root_pem(..)",
                    server.addr(),
                    server.host(),
                )
            })?;

            tracing::debug!(server = %server, "TLS handshake complete");

            let (read_half, write_half) = tokio::io::split(stream);
            Ok(Connection {
                reader: ReadHalf::Tls(read_half),
                writer: WriteHalf::Tls(write_half),
                #[cfg(unix)]
                raw_fd: None,
            })
        }

        // Without the `tls` feature `TlsSettings` is uninhabited, so this arm is
        // unreachable by construction — an empty match proves it to the compiler
        // without a runtime panic.
        #[cfg(not(feature = "tls"))]
        Some(tls) => match *tls {},
    }
}

/// Rebuild a [`Connection`] from a file descriptor inherited across `exec`.
///
/// Only ever a plaintext socket; see [`Connection::raw_fd`].
///
/// # Errors
///
/// Returns an error if the descriptor cannot be turned into a Tokio
/// [`TcpStream`] — for instance if it was already closed, or does not refer to
/// a socket.
#[cfg(unix)]
pub(crate) fn from_inherited_fd(raw_fd: std::os::unix::io::RawFd) -> Result<Connection, BoxError> {
    use std::os::unix::io::FromRawFd;

    // Safety: the fd was inherited from the process that called `exec_reload`
    // and has not been closed since — `exec` preserves descriptors without
    // FD_CLOEXEC, and nothing in this process has touched it yet.
    let std_stream = unsafe { std::net::TcpStream::from_raw_fd(raw_fd) };
    std_stream.set_nonblocking(true).map_err(|e| {
        format!("failed to make the inherited socket (fd {raw_fd}) non-blocking: {e}")
    })?;
    let stream = TcpStream::from_std(std_stream)
        .map_err(|e| format!("failed to adopt the inherited socket (fd {raw_fd}): {e}"))?;

    let (read_half, write_half) = stream.into_split();
    Ok(Connection {
        reader: ReadHalf::Plain(read_half),
        writer: WriteHalf::Plain(write_half),
        raw_fd: Some(raw_fd),
    })
}

// ─── halves ──────────────────────────────────────────────────────────────────

/// The reading half of a connection.
pub(crate) enum ReadHalf {
    Plain(tokio::net::tcp::OwnedReadHalf),
    #[cfg(feature = "tls")]
    Tls(tokio::io::ReadHalf<tokio_rustls::client::TlsStream<TcpStream>>),
}

/// The writing half of a connection.
pub(crate) enum WriteHalf {
    Plain(tokio::net::tcp::OwnedWriteHalf),
    #[cfg(feature = "tls")]
    Tls(tokio::io::WriteHalf<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for ReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            ReadHalf::Plain(h) => Pin::new(h).poll_read(cx, buf),
            #[cfg(feature = "tls")]
            ReadHalf::Tls(h) => Pin::new(h).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for WriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            WriteHalf::Plain(h) => Pin::new(h).poll_write(cx, buf),
            #[cfg(feature = "tls")]
            WriteHalf::Tls(h) => Pin::new(h).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            WriteHalf::Plain(h) => Pin::new(h).poll_flush(cx),
            #[cfg(feature = "tls")]
            WriteHalf::Tls(h) => Pin::new(h).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            WriteHalf::Plain(h) => Pin::new(h).poll_shutdown(cx),
            #[cfg(feature = "tls")]
            WriteHalf::Tls(h) => Pin::new(h).poll_shutdown(cx),
        }
    }
}

#[cfg(all(test, feature = "tls"))]
mod tls_tests {
    use std::sync::Arc;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio_rustls::rustls::{crypto::ring, ServerConfig};
    use tokio_rustls::TlsAcceptor;

    use crate::{Server, State};

    /// A self-signed certificate for `localhost`, plus its PEM encoding for the
    /// client to trust.
    struct TestCert {
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        pem: String,
    }

    fn self_signed() -> TestCert {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("failed to generate a test certificate");
        TestCert {
            chain: vec![generated.cert.der().clone()],
            key: PrivateKeyDer::try_from(generated.signing_key.serialize_der())
                .expect("generated key should be valid DER"),
            pem: generated.cert.pem(),
        }
    }

    /// Start a TLS listener on loopback that reads the client's registration
    /// lines and reports the first one it decrypts.
    ///
    /// Returns the address to connect to and a receiver that yields the first
    /// line the server read, proving the encrypted stream round-tripped.
    async fn spawn_tls_server(cert: TestCert) -> (String, oneshot::Receiver<String>) {
        let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("default protocol versions should be supported")
            .with_no_client_auth()
            .with_single_cert(cert.chain, cert.key)
            .expect("generated certificate and key should form a usable pair");
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = oneshot::channel();

        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept failed");
            let stream = match acceptor.accept(socket).await {
                Ok(stream) => stream,
                Err(e) => panic!("server-side TLS handshake failed: {e}"),
            };
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .expect("failed to read from the TLS stream");
            let _ = tx.send(line.trim_end().to_string());

            // Hold the connection open so the client isn't torn down mid-test.
            let _ = reader.get_mut().write_all(b"PING :hold\r\n").await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });

        (addr, rx)
    }

    /// Given a server presenting a self-signed certificate, when that exact
    /// certificate is trusted via `with_extra_root_pem`, then the handshake
    /// succeeds and the IRC registration arrives encrypted.
    #[tokio::test]
    async fn connects_over_tls_when_the_certificate_is_trusted() {
        let cert = self_signed();
        let pem = cert.pem.clone();
        let (addr, first_line) = spawn_tls_server(cert).await;

        let state = State::connect(
            "tester",
            Server::tls(&addr)
                .with_sni("localhost")
                .with_extra_root_pem(pem.into_bytes()),
            vec![],
        )
        .await
        .expect("TLS connect should succeed against a trusted certificate");

        assert!(state.server.is_tls());
        assert_eq!(
            first_line
                .await
                .expect("server task ended without reporting"),
            "NICK tester"
        );
    }

    /// A TLS connection cannot be inherited across `exec`, so it must not offer
    /// a descriptor for the hot-reload path to hand over.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_tls_connection_exposes_no_fd_for_hot_reload() {
        let cert = self_signed();
        let pem = cert.pem.clone();
        let (addr, _first_line) = spawn_tls_server(cert).await;

        let state = State::connect(
            "tester",
            Server::tls(&addr)
                .with_sni("localhost")
                .with_extra_root_pem(pem.into_bytes()),
            vec![],
        )
        .await
        .expect("TLS connect should succeed");

        assert_eq!(state.raw_fd, None);
    }

    /// A plaintext connection is still hot-reloadable.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_plaintext_connection_exposes_its_fd_for_hot_reload() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _sock = listener.accept().await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });

        let state = State::connect("tester", addr.as_str(), vec![])
            .await
            .expect("plaintext connect should succeed");

        assert!(state.raw_fd.is_some());
    }

    /// Given a server presenting a self-signed certificate, when it is not
    /// trusted, then the handshake fails rather than silently proceeding.
    #[tokio::test]
    async fn rejects_an_untrusted_certificate() {
        let cert = self_signed();
        let (addr, _first_line) = spawn_tls_server(cert).await;

        // `State` is not `Debug`, so `expect_err` is unavailable.
        let err = match State::connect("tester", Server::tls(&addr).with_sni("localhost"), vec![])
            .await
        {
            Ok(_) => panic!("an untrusted self-signed certificate must not be accepted"),
            Err(e) => e.to_string(),
        };

        assert!(
            err.contains("with_extra_root_pem"),
            "the error should point at the fix: {err}"
        );
    }

    /// `danger_accept_invalid_certs` is the documented escape hatch, so it has
    /// to actually work on the same untrusted certificate.
    #[tokio::test]
    async fn danger_accept_invalid_certs_allows_an_untrusted_certificate() {
        let cert = self_signed();
        let (addr, first_line) = spawn_tls_server(cert).await;

        let state = State::connect(
            "tester",
            Server::tls(&addr).danger_accept_invalid_certs(),
            vec![],
        )
        .await
        .expect("verification is disabled, so the handshake should succeed");

        assert!(state.server.is_tls());
        assert_eq!(
            first_line
                .await
                .expect("server task ended without reporting"),
            "NICK tester"
        );
    }
}
