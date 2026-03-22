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
///    keepalive settings) into environment variables.
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

    let err = std::process::Command::new(&exe)
        .env(ENV_FD, raw_fd.to_string())
        .env(ENV_NICK, nick)
        .env(ENV_SERVER, server)
        .env(ENV_CHANNELS, channels.join(","))
        .env(ENV_KA_INTERVAL, keepalive_interval_ms.to_string())
        .env(ENV_KA_TIMEOUT, keepalive_timeout_ms.to_string())
        .exec(); // never returns on success

    Box::new(err)
}

// ─── env var names ────────────────────────────────────────────────────────────

pub const ENV_FD: &str = "IRCBOT_INHERIT_FD";
pub const ENV_NICK: &str = "IRCBOT_NICK";
pub const ENV_SERVER: &str = "IRCBOT_SERVER";
pub const ENV_CHANNELS: &str = "IRCBOT_CHANNELS";
pub const ENV_KA_INTERVAL: &str = "IRCBOT_KEEPALIVE_INTERVAL_MS";
pub const ENV_KA_TIMEOUT: &str = "IRCBOT_KEEPALIVE_TIMEOUT_MS";
