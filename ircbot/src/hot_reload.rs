//! Hot-reload support: replace the running bot binary without dropping the
//! IRC connection.
//!
//! The functions in this module are Unix-only. On other targets only the
//! `ENV_*` constants remain, and the `#[bot]` macro omits the `SIGHUP` listener
//! that drives a reload.
//!
//! # How it works
//!
//! On Unix, a TCP socket is just an open file descriptor.  When a process
//! calls `exec()` the new process image inherits all file descriptors that do
//! **not** have the `FD_CLOEXEC` flag set.
//!
//! [`exec_reload`] exploits this:
//!
//! 1. Clears `FD_CLOEXEC` on the live TCP socket fd so the new binary
//!    inherits it.
//! 2. Serialises the connection metadata (fd number, nick, server, channels,
//!    keepalive settings, and — when recorded via [`record_flood_settings`] —
//!    flood-control settings) into environment variables.
//! 3. Calls `exec()` to replace the current process image with the new
//!    binary.  The PID does not change, and the TCP connection is never closed.
//!
//! The new binary calls [`crate::connection::State::try_inherit_from_env`]
//! at startup.  If the env vars are present it reconstructs a live `State`
//! from the inherited fd instead of opening a new TCP connection.
//!
//! # TLS connections
//!
//! A `raw_fd` of `None` means the connection cannot be handed over — the case
//! for TLS. A TLS session's keys, record sequence numbers, and any
//! partially-read record live in this process's memory, which `exec` discards.
//! The successor would inherit a socket it has no way to decrypt. The binary is
//! still replaced, just without the socket, so the new process connects afresh
//! and rejoins. Callers must warn the user that the connection will drop.

#[cfg(unix)]
use std::sync::atomic::Ordering;

/// Replace the current process image with a new build of the same binary,
/// handing over the live IRC socket when it can be inherited.
///
/// Pass `raw_fd` as `None` to replace the binary without the socket. The
/// successor then opens a fresh connection and rejoins `channels`.
///
/// A `Some` value says to hand the socket over, not which one: the descriptor
/// that goes to the successor is the one the live connection recorded with
/// [`record_fd`], because a reconnect replaces the socket while the caller
/// keeps the descriptor it first captured. The passed value is used only when
/// no connection recorded itself.
///
/// On success this function does not return: the process image is gone.
///
/// # Errors
///
/// Returns the error that stopped the reload. Because `exec()` only returns on
/// failure, a returned value always means the current process is still running
/// the old binary and still holds the connection.
#[cfg(unix)]
pub fn exec_reload(
    raw_fd: Option<std::os::unix::io::RawFd>,
    nick: &str,
    server: &str,
    channels: &[String],
    keepalive_interval_ms: u64,
    keepalive_timeout_ms: u64,
) -> crate::BoxError {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::process::CommandExt;

    // Take a copy of the live descriptor and hand that over. The read loop can
    // close its own copy at any moment, on another runtime thread, and the
    // copy keeps the socket open until the `exec`. The copy owns itself, so
    // every path that returns from here closes it; a successful `exec` runs no
    // destructor and the successor inherits it.
    let socket = fd_for_reload(raw_fd, recorded_fd()).and_then(dup_for_exec);

    if socket.is_none() {
        tracing::warn!(
            %server,
            "hot reload: there is no socket to hand over, because the bot is between two \
             connections, or the connection ended as this reload started, or it cannot survive \
             the exec (a TLS session cannot), so the new binary will connect and rejoin — \
             expect a brief disconnect"
        );
    }

    // `dup` returns its copy with FD_CLOEXEC clear, so the copy survives the
    // `exec` whatever the flag was on the descriptor it came from.

    // Encode state into env vars for the new process.
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return Box::new(e),
    };

    let mut cmd = std::process::Command::new(&exe);
    cmd.env(ENV_NICK, nick)
        .env(ENV_SERVER, server)
        .env(ENV_CHANNELS, channels.join(","))
        .env(ENV_KA_INTERVAL, keepalive_interval_ms.to_string())
        .env(ENV_KA_TIMEOUT, keepalive_timeout_ms.to_string());

    // Without ENV_FD the successor's `try_inherit_from_env` returns `None` and
    // it opens a fresh connection instead of adopting this one.
    if let Some(fd) = &socket {
        cmd.env(ENV_FD, fd.as_raw_fd().to_string());
    }

    // The `#[bot]`-generated `main_loop` forwards keepalive timings as arguments
    // but, for compatibility with already-published macro versions, does not
    // pass the flood-control settings.  `run_bot` records them via
    // `record_flood_settings` so they can still be serialised here (and stay
    // child-scoped, like the other vars, rather than leaking to the process env).
    if let Some((burst, rate_ms)) = FLOOD_FOR_RELOAD.get() {
        cmd.env(ENV_FLOOD_BURST, burst.to_string())
            .env(ENV_FLOOD_RATE, rate_ms.to_string());
    }

    // A `SIGHUP` can arrive between the connect and `RPL_WELCOME`, so the
    // successor cannot assume that the socket it inherits is on the network.
    let registered = REGISTERED_FOR_RELOAD.load(Ordering::Relaxed);
    cmd.env(ENV_REGISTERED, if registered { "1" } else { "0" });

    let err = cmd.exec(); // never returns on success

    // The exec failed, so this process keeps running and keeps its own socket.
    // Dropping `socket` closes the copy rather than leaving it open for the
    // life of the process.
    Box::new(err)
}

/// Copy `raw_fd`, so that the socket stays open while this reload prepares
/// the `exec` even if the read loop closes its own copy.
///
/// Returns `None` when the descriptor is already gone. The successor then
/// opens its own connection, which is what a bot without an inherited socket
/// does anyway.
///
/// The copy also keeps `FD_CLOEXEC` off the descriptor of the bot: `dup`
/// returns the copy with the flag clear, and an `exec` that never happens
/// leaves the socket of the bot as it was.
#[cfg(unix)]
fn dup_for_exec(raw_fd: std::os::unix::io::RawFd) -> Option<std::os::unix::io::OwnedFd> {
    use std::os::unix::io::FromRawFd;

    // Safety: `dup` reads no memory. It returns -1 when it cannot copy the
    // descriptor, which is the case this function reports as `None`.
    let copy = unsafe { libc::dup(raw_fd) };
    if copy == -1 {
        // The descriptor is closed, the process or the system is out of
        // descriptors, or the call was interrupted. The error says which.
        tracing::warn!(
            fd = raw_fd,
            error = %std::io::Error::last_os_error(),
            "hot reload: could not copy the socket for the hand-over",
        );
        return None;
    }
    // Safety: `dup` returned a new descriptor that nothing else owns.
    Some(unsafe { std::os::unix::io::OwnedFd::from_raw_fd(copy) })
}

/// Record the active flood-control settings (`burst`, `rate` in milliseconds)
/// so a subsequent [`exec_reload`] can carry them to the successor process.
///
/// Called once by [`crate::internal::run_bot`] at start-up.  This indirection
/// exists because the `#[bot]` macro — pinned to a published version — does not
/// forward flood settings to `exec_reload` directly.  Only the first value is
/// retained; later calls (e.g. on reconnect) are ignored.
#[cfg(unix)]
pub fn record_flood_settings(burst: usize, rate_ms: u64) {
    let _ = FLOOD_FOR_RELOAD.set((burst, rate_ms));
}

/// Flood-control settings stashed by [`record_flood_settings`] for [`exec_reload`].
#[cfg(unix)]
static FLOOD_FOR_RELOAD: std::sync::OnceLock<(usize, u64)> = std::sync::OnceLock::new();

/// Record whether the current connection is registered, so a subsequent
/// [`exec_reload`] can tell the successor what it inherits.
///
/// Three places call this, and together they keep the value true of the
/// connection of the moment: `State::connect` records `false` for a socket
/// that has not registered yet, `State::try_inherit_from_env` records what the
/// predecessor process wrote, and the read loop records `true` at
/// `RPL_WELCOME`. The first two run before the `SIGHUP` listener of the
/// `#[bot]` macro exists, which is what a reload arriving at once depends on.
/// Keep it that way: a call that waits for the read loop to start leaves a
/// window where this value is the default rather than the state of the socket.
///
/// The last call wins, unlike [`record_flood_settings`], which keeps the first
/// value. It is process-wide, as the reload is: it describes the connection
/// that this process hands over.
#[cfg(unix)]
pub fn record_registration(registered: bool) {
    REGISTERED_FOR_RELOAD.store(registered, Ordering::Relaxed);
}

/// Record the descriptor of the connection that this process now holds, so a
/// subsequent [`exec_reload`] hands over the live socket.
///
/// `State::connect` and `State::try_inherit_from_env` call this when they make
/// or adopt a connection, which includes every reconnect. The read loop calls
/// it with `None` when a connection ends, and so does a connection that cannot
/// be inherited, as a TLS session cannot. The successor then opens its own
/// connection, and a caller of [`exec_reload`] that asked to hand a socket
/// over gets no socket rather than a closed one.
///
/// It is process-wide, as the reload is: it describes the connection that this
/// process hands over.
#[cfg(unix)]
pub fn record_fd(raw_fd: Option<std::os::unix::io::RawFd>) {
    FD_FOR_RELOAD.store(raw_fd.unwrap_or(NO_SOCKET), Ordering::Relaxed);
}

/// The descriptor to hand to the successor.
///
/// The caller of [`exec_reload`] is the `SIGHUP` task of the `#[bot]` macro,
/// which captured `caller` before the bot started. Every reconnect opens a new
/// socket with a new descriptor, and between two connections there is none at
/// all, so a caller that asks to hand a socket over gets what the connection
/// recorded. A caller that passed `None` asked for no socket, and keeps that.
#[cfg(unix)]
fn fd_for_reload(
    caller: Option<std::os::unix::io::RawFd>,
    recorded: Option<Option<std::os::unix::io::RawFd>>,
) -> Option<std::os::unix::io::RawFd> {
    match caller {
        Some(captured) => recorded.unwrap_or(Some(captured)),
        None => None,
    }
}

/// What [`record_fd`] last recorded.
///
/// `None` when nothing was recorded, which leaves the caller of
/// [`exec_reload`] with the descriptor it passed. `Some(None)` is a connection
/// that ended, or one that cannot be inherited.
#[cfg(unix)]
fn recorded_fd() -> Option<Option<std::os::unix::io::RawFd>> {
    decode_fd(FD_FOR_RELOAD.load(Ordering::Relaxed))
}

/// The meaning of a value in [`FD_FOR_RELOAD`], kept apart from the static so
/// it can be tested.
#[cfg(unix)]
fn decode_fd(recorded: std::os::unix::io::RawFd) -> Option<Option<std::os::unix::io::RawFd>> {
    match recorded {
        UNRECORDED => None,
        NO_SOCKET => Some(None),
        fd => Some(Some(fd)),
    }
}

/// Descriptor of the live connection, stashed by [`record_fd`] for
/// [`exec_reload`]. A descriptor is never negative, so the two states that are
/// not a descriptor use negative values.
#[cfg(unix)]
static FD_FOR_RELOAD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(UNRECORDED);

/// [`FD_FOR_RELOAD`] before any connection recorded itself.
#[cfg(unix)]
const UNRECORDED: std::os::unix::io::RawFd = i32::MIN;

/// [`FD_FOR_RELOAD`] when there is no socket to hand over: the connection
/// ended, or it cannot survive an `exec`.
#[cfg(unix)]
const NO_SOCKET: std::os::unix::io::RawFd = -1;

/// Registration state stashed by [`record_registration`] for [`exec_reload`].
#[cfg(unix)]
static REGISTERED_FOR_RELOAD: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// ─── env var names ────────────────────────────────────────────────────────────
//
// exec_reload writes these into the environment of the successor process, and
// State::try_inherit_from_env reads them back.

/// Holds the file descriptor number of the inherited socket. When this variable
/// is absent, the successor opens a new connection.
pub const ENV_FD: &str = "IRCBOT_INHERIT_FD";
/// Holds the nick the predecessor was registered with.
pub const ENV_NICK: &str = "IRCBOT_NICK";
/// Holds the server address the predecessor was connected to.
pub const ENV_SERVER: &str = "IRCBOT_SERVER";
/// Holds the joined channels, separated by commas.
pub const ENV_CHANNELS: &str = "IRCBOT_CHANNELS";
/// Holds the keepalive interval, in milliseconds.
pub const ENV_KA_INTERVAL: &str = "IRCBOT_KEEPALIVE_INTERVAL_MS";
/// Holds the keepalive timeout, in milliseconds.
pub const ENV_KA_TIMEOUT: &str = "IRCBOT_KEEPALIVE_TIMEOUT_MS";
/// Holds the flood-control burst size. Absent when the predecessor never called
/// [`record_flood_settings`], and the successor then uses the default.
pub const ENV_FLOOD_BURST: &str = "IRCBOT_FLOOD_BURST";
/// Holds the flood-control rate, in milliseconds per message. Absent under the
/// same conditions as [`ENV_FLOOD_BURST`].
pub const ENV_FLOOD_RATE: &str = "IRCBOT_FLOOD_RATE_MS";
/// Holds `"1"` when the inherited connection completed registration before the
/// reload, and `"0"` when it did not. Absent when the predecessor is a version
/// from before this variable, which handed over the socket whatever its state;
/// the successor then reads the connection as registered, as it did then.
pub const ENV_REGISTERED: &str = "IRCBOT_REGISTERED";

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    // ── decode_fd ──────────────────────────────────────────────────────────────

    #[test]
    fn nothing_recorded_leaves_the_caller_with_its_own_descriptor() {
        assert_eq!(decode_fd(UNRECORDED), None);
    }

    #[test]
    fn a_connection_that_cannot_be_inherited_records_no_descriptor() {
        assert_eq!(decode_fd(NO_SOCKET), Some(None));
    }

    #[test]
    fn a_recorded_descriptor_reads_back_as_itself() {
        assert_eq!(decode_fd(7), Some(Some(7)));
    }

    // ── dup_for_exec ───────────────────────────────────────────────────────────

    /// A connected loopback socket, and the descriptor it owns.
    fn connected_socket() -> std::net::TcpStream {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let _held = listener.accept();
            std::thread::sleep(std::time::Duration::from_millis(500));
        });
        std::net::TcpStream::connect(addr).expect("connect failed")
    }

    #[test]
    fn a_copy_of_a_live_socket_outlives_the_original() {
        use std::os::unix::io::AsRawFd;

        let socket = connected_socket();
        let original = socket.as_raw_fd();

        let copy = dup_for_exec(original).expect("a live socket can be copied");
        drop(socket);

        assert_ne!(copy.as_raw_fd(), original, "the copy is its own descriptor");
        assert_ne!(
            unsafe { libc::fcntl(copy.as_raw_fd(), libc::F_GETFD) },
            -1,
            "the copy is still open after the original closed",
        );
        // Dropping `copy` closes it, as it does in `exec_reload`.
    }

    #[test]
    fn the_copy_survives_an_exec() {
        use std::os::unix::io::AsRawFd;

        let socket = connected_socket();

        let copy = dup_for_exec(socket.as_raw_fd()).expect("a live socket can be copied");

        let flags = unsafe { libc::fcntl(copy.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags, -1, "the copy is open");
        assert_eq!(
            flags & libc::FD_CLOEXEC,
            0,
            "the copy is the one descriptor that an exec keeps",
        );
    }

    #[test]
    fn a_descriptor_that_is_not_open_cannot_be_copied() {
        // A socket that closed and a descriptor that was never open are the
        // same thing to `dup`, and this one cannot be given to another test
        // while this one runs.
        assert!(dup_for_exec(-1).is_none());
    }

    // ── fd_for_reload ──────────────────────────────────────────────────────────

    #[test]
    fn the_live_descriptor_replaces_the_one_the_caller_captured() {
        assert_eq!(fd_for_reload(Some(3), Some(Some(9))), Some(9));
    }

    #[test]
    fn a_connection_that_ended_hands_over_no_descriptor() {
        // Between two connections the captured descriptor is closed, and its
        // number can already belong to something else.
        assert_eq!(fd_for_reload(Some(3), Some(None)), None);
    }

    #[test]
    fn the_caller_keeps_its_descriptor_when_no_connection_recorded_one() {
        assert_eq!(fd_for_reload(Some(3), None), Some(3));
    }

    #[test]
    fn a_caller_that_asks_for_no_socket_hands_over_none() {
        assert_eq!(fd_for_reload(None, Some(Some(9))), None);
        assert_eq!(fd_for_reload(None, None), None);
    }
}
