/// Hot-reload support: replace the running bot binary without dropping the
/// IRC connection.
///
/// # How it works
///
/// On Unix, a TCP socket is just an open file descriptor.  When a process
/// calls `exec()` the new process image inherits all file descriptors that do
/// **not** have the `FD_CLOEXEC` flag set.
///
/// [`exec_reload`] exploits this:
///
/// 1. Clears `FD_CLOEXEC` on the live TCP socket fd so the new binary
///    inherits it.
/// 2. Serialises the connection metadata (fd number, nick, server, channels,
///    keepalive settings, and — when recorded via [`record_flood_settings`] —
///    flood-control settings) into environment variables.
/// 3. Calls `exec()` to replace the current process image with the new
///    binary.  The PID does not change; the TCP connection is never closed.
///
/// The new binary calls [`crate::connection::State::try_inherit_from_env`]
/// at startup.  If the env vars are present it reconstructs a live `State`
/// from the inherited fd instead of opening a new TCP connection.
///
/// This is the Unix-only implementation.  On non-Unix targets the function is
/// a no-op that returns `Err`.
#[cfg(unix)]
pub fn exec_reload(
    raw_fd: std::os::unix::io::RawFd,
    nick: &str,
    server: &str,
    channels: &[String],
    keepalive_interval_ms: u64,
    keepalive_timeout_ms: u64,
) -> crate::BoxError {
    use std::os::unix::process::CommandExt;

    // Clear FD_CLOEXEC so the fd survives exec.
    let flags = unsafe { libc::fcntl(raw_fd, libc::F_GETFD) };
    if flags == -1 {
        return format!("fcntl(F_GETFD) failed: {}", std::io::Error::last_os_error()).into();
    }
    let rc = unsafe { libc::fcntl(raw_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) };
    if rc == -1 {
        return format!("fcntl(F_SETFD) failed: {}", std::io::Error::last_os_error()).into();
    }

    // Encode state into env vars for the new process.
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return Box::new(e),
    };

    let mut cmd = std::process::Command::new(&exe);
    cmd.env(ENV_FD, raw_fd.to_string())
        .env(ENV_NICK, nick)
        .env(ENV_SERVER, server)
        .env(ENV_CHANNELS, channels.join(","))
        .env(ENV_KA_INTERVAL, keepalive_interval_ms.to_string())
        .env(ENV_KA_TIMEOUT, keepalive_timeout_ms.to_string());

    // The `#[bot]`-generated `main_loop` forwards keepalive timings as arguments
    // but, for compatibility with already-published macro versions, does not
    // pass the flood-control settings.  `run_bot` records them via
    // `record_flood_settings` so they can still be serialised here (and stay
    // child-scoped, like the other vars, rather than leaking to the process env).
    if let Some((burst, rate_ms)) = FLOOD_FOR_RELOAD.get() {
        cmd.env(ENV_FLOOD_BURST, burst.to_string())
            .env(ENV_FLOOD_RATE, rate_ms.to_string());
    }

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

// ─── env var names ────────────────────────────────────────────────────────────

pub const ENV_FD: &str = "IRCBOT_INHERIT_FD";
pub const ENV_NICK: &str = "IRCBOT_NICK";
pub const ENV_SERVER: &str = "IRCBOT_SERVER";
pub const ENV_CHANNELS: &str = "IRCBOT_CHANNELS";
pub const ENV_KA_INTERVAL: &str = "IRCBOT_KEEPALIVE_INTERVAL_MS";
pub const ENV_KA_TIMEOUT: &str = "IRCBOT_KEEPALIVE_TIMEOUT_MS";
pub const ENV_FLOOD_BURST: &str = "IRCBOT_FLOOD_BURST";
pub const ENV_FLOOD_RATE: &str = "IRCBOT_FLOOD_RATE_MS";
