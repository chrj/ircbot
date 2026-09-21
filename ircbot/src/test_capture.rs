//! A `tracing` writer that keeps what the subscriber emitted, so a test can
//! read it back.
//!
//! Test-only: the crate installs no subscriber of its own, and a test that
//! asserts on a log line installs one for its own thread with
//! `tracing::subscriber::set_default`. `#[tokio::test]` runs a current-thread
//! runtime, so the tasks the test spawns share that subscriber.

/// Appends everything it is handed to a shared buffer.
#[derive(Clone, Default)]
pub(crate) struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl CaptureWriter {
    /// Everything written so far.
    pub(crate) fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .expect("capture buffer is valid UTF-8")
    }
}

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
