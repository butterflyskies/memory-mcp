//! Test-only tracing capture helpers shared across unit-test modules.

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use tracing::subscriber::with_default;
use tracing_subscriber::{layer::SubscriberExt, Registry};

/// A `Write` sink that appends into a shared buffer.
struct TestWriter(Arc<Mutex<Vec<u8>>>);

impl Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `f` with an INFO-level subscriber installed on the current thread and
/// return everything it logged as a string.
pub(crate) fn capture_info_logs(f: impl FnOnce()) -> String {
    let output = Arc::new(Mutex::new(Vec::new()));
    let writer_output = Arc::clone(&output);
    let subscriber = Registry::default()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(move || TestWriter(Arc::clone(&writer_output))),
        );
    with_default(subscriber, f);
    let bytes = output.lock().expect("log buffer").clone();
    String::from_utf8(bytes).expect("UTF-8 logs")
}
