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
    use std::os::unix::process::CommandExt;

    if raw_fd.is_none() {
        tracing::warn!(
            %server,
            "hot reload: this connection cannot be inherited across exec (TLS sessions do not \
             survive it), so the new binary will reconnect and rejoin — expect a brief disconnect"
        );
    }

    // The caller of this function is the `SIGHUP` task of the `#[bot]` macro,
    // which captured its descriptor before the bot started. Every reconnect
    // opens a new socket with a new descriptor, so a caller that wants the
    // socket handed over gets the one the connection recorded last. A caller
    // that passed `None` wants no socket handed over, and keeps that.
    let raw_fd = match raw_fd {
        Some(captured) => recorded_fd().unwrap_or(Some(captured)),
        None => None,
    };

    // Clear FD_CLOEXEC so the fd survives exec.
    if let Some(fd) = raw_fd {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags == -1 {
            return format!("fcntl(F_GETFD) failed: {}", std::io::Error::last_os_error()).into();
        }
        let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
        if rc == -1 {
            return format!("fcntl(F_SETFD) failed: {}", std::io::Error::last_os_error()).into();
        }
    }

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
    if let Some(fd) = raw_fd {
        cmd.env(ENV_FD, fd.to_string());
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

    Box::new(err)
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
/// or adopt a connection, which includes every reconnect. Pass `None` for a
/// connection that cannot be inherited, as a TLS session cannot: the successor
/// then opens its own connection, and a caller of [`exec_reload`] that asked
/// to hand a socket over gets no socket rather than a stale one.
///
/// It is process-wide, as the reload is: it describes the connection that this
/// process hands over.
#[cfg(unix)]
pub fn record_fd(raw_fd: Option<std::os::unix::io::RawFd>) {
    FD_FOR_RELOAD.store(raw_fd.unwrap_or(NOT_INHERITABLE), Ordering::Relaxed);
}

/// What [`record_fd`] last recorded.
///
/// `None` when nothing was recorded, which leaves the caller of
/// [`exec_reload`] with the descriptor it passed. `Some(None)` is a connection
/// that cannot be inherited.
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
        NOT_INHERITABLE => Some(None),
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

/// [`FD_FOR_RELOAD`] for a connection that cannot survive an `exec`.
#[cfg(unix)]
const NOT_INHERITABLE: std::os::unix::io::RawFd = -1;

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
        assert_eq!(decode_fd(NOT_INHERITABLE), Some(None));
    }

    #[test]
    fn a_recorded_descriptor_reads_back_as_itself() {
        assert_eq!(decode_fd(7), Some(Some(7)));
    }
}
