//! Tracing output that is redacted before it is written anywhere.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

use super::redact::redact;

/// Rotate `path` to `path.1` when it is larger than this at start-up.
pub const ROTATE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone)]
enum Sink {
    File(Arc<Mutex<File>>),
    Stderr,
}

/// A `MakeWriter` whose writers buffer one event and write it redacted.
#[derive(Clone)]
pub struct RedactingWriter {
    sink: Sink,
}

impl RedactingWriter {
    /// Append to `path` (rotating it first if it is too large).
    pub fn file(path: &Path) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if std::fs::metadata(path).is_ok_and(|m| m.len() > ROTATE_BYTES) {
            let rotated = PathBuf::from(format!("{}.1", path.display()));
            let _ = std::fs::remove_file(&rotated);
            let _ = std::fs::rename(path, &rotated);
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            sink: Sink::File(Arc::new(Mutex::new(file))),
        })
    }

    pub fn stderr() -> Self {
        Self { sink: Sink::Stderr }
    }
}

pub struct EventWriter {
    buf: Vec<u8>,
    sink: Sink,
}

impl Write for EventWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for EventWriter {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let text = redact(&String::from_utf8_lossy(&self.buf));
        match &self.sink {
            Sink::File(file) => {
                let mut file = file.lock().unwrap_or_else(|e| e.into_inner());
                let _ = file.write_all(text.as_bytes());
            }
            Sink::Stderr => {
                let _ = std::io::stderr().write_all(text.as_bytes());
            }
        }
    }
}

impl<'a> MakeWriter<'a> for RedactingWriter {
    type Writer = EventWriter;

    fn make_writer(&'a self) -> Self::Writer {
        EventWriter {
            buf: Vec::new(),
            sink: self.sink.clone(),
        }
    }
}

/// Last `max_lines` lines of a log file (for diagnostics), already redacted
/// at write time and redacted again defensively.
pub fn tail(path: &Path, max_lines: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].iter().map(|l| redact(l)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_writer_redacts_and_tail_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("test.log");
        let writer = RedactingWriter::file(&path).unwrap();
        {
            let mut w = writer.make_writer();
            w.write_all(b"mounting with token hf_abcdefghijklmnopqrstuvwxyz\n")
                .unwrap();
        }
        {
            let mut w = writer.make_writer();
            w.write_all(b"second line\n").unwrap();
        }
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("abcdefghijklmnop"), "{content}");
        assert_eq!(tail(&path, 1), vec!["second line".to_string()]);
        assert_eq!(tail(&path, 10).len(), 2);
    }

    #[test]
    fn large_logs_rotate_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.log");
        std::fs::write(&path, vec![b'x'; (ROTATE_BYTES + 1) as usize]).unwrap();
        let _w = RedactingWriter::file(&path).unwrap();
        assert!(dir.path().join("big.log.1").exists());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    }
}
