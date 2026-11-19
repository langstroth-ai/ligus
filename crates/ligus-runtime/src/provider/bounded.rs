//! Bounded reads of an agent child's output. A CLI agent (or an ACP adapter)
//! is untrusted input to the host: a line without a newline, or an endless
//! stream, would otherwise grow the host's buffers until it ran out of
//! memory. Every adapter reads stdout through [`BoundedLines`] (one line of
//! at most [`MAX_LINE_BYTES`], at most [`MAX_TOTAL_BYTES`] per turn) and
//! stderr through [`read_capped`]; on overflow the turn ends with a clear
//! error.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt};

/// The longest stdout line a turn may produce (`LIGUS_AGENT_MAX_LINE_BYTES`).
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// The most stdout a turn may produce in all (`LIGUS_AGENT_MAX_OUTPUT_BYTES`).
pub const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
/// How much of stderr is kept for error messages (the rest is drained).
pub const MAX_STDERR_BYTES: usize = 256 * 1024;

fn env_limit(name: &str, default: usize) -> usize {
    ligus_core::compat::var(name).ok().and_then(|v| v.trim().parse().ok()).filter(|n: &usize| *n > 0).unwrap_or(default)
}

/// The per-line cap in force.
pub fn max_line_bytes() -> usize {
    env_limit("LIGUS_AGENT_MAX_LINE_BYTES", MAX_LINE_BYTES)
}

/// The per-turn cap in force.
pub fn max_total_bytes() -> usize {
    env_limit("LIGUS_AGENT_MAX_OUTPUT_BYTES", MAX_TOTAL_BYTES)
}

/// Why a read stopped.
#[derive(Debug)]
pub enum LineError {
    Io(std::io::Error),
    /// A line longer than the cap.
    LineTooLong(usize),
    /// More output than the per-turn cap.
    TooMuchOutput(usize),
}

impl std::fmt::Display for LineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LineError::Io(e) => write!(f, "read: {e}"),
            LineError::LineTooLong(n) => write!(f, "the agent wrote an output line longer than {} MiB; the turn was stopped (LIGUS_AGENT_MAX_LINE_BYTES)", n / (1024 * 1024)),
            LineError::TooMuchOutput(n) => write!(f, "the agent wrote more than {} MiB of output; the turn was stopped (LIGUS_AGENT_MAX_OUTPUT_BYTES)", n / (1024 * 1024)),
        }
    }
}

/// Newline-delimited lines with a per-line and a total cap. Drop-in for
/// `tokio::io::Lines::next_line` (cancel-safe in the same way: a partial
/// line stays buffered across calls).
pub struct BoundedLines<R> {
    inner: R,
    buf: Vec<u8>,
    total: usize,
    max_line: usize,
    max_total: usize,
}

impl<R: AsyncBufRead + Unpin> BoundedLines<R> {
    pub fn new(inner: R) -> Self {
        Self::with_limits(inner, max_line_bytes(), max_total_bytes())
    }

    pub fn with_limits(inner: R, max_line: usize, max_total: usize) -> Self {
        Self { inner, buf: Vec::new(), total: 0, max_line, max_total }
    }

    /// The next line without its `\n` (and `\r`), `None` at end of stream.
    /// Invalid UTF-8 is replaced, as `String::from_utf8_lossy` does.
    pub async fn next_line(&mut self) -> Result<Option<String>, LineError> {
        loop {
            let avail = self.inner.fill_buf().await.map_err(LineError::Io)?;
            if avail.is_empty() {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(self.take_line()));
            }
            let (chunk, done) = match avail.iter().position(|b| *b == b'\n') {
                Some(i) => (&avail[..i], Some(i + 1)),
                None => (avail, None),
            };
            if self.buf.len() + chunk.len() > self.max_line {
                return Err(LineError::LineTooLong(self.max_line));
            }
            self.buf.extend_from_slice(chunk);
            let used = done.unwrap_or(avail.len());
            self.inner.consume(used);
            self.total = self.total.saturating_add(used);
            if self.total > self.max_total {
                return Err(LineError::TooMuchOutput(self.max_total));
            }
            if done.is_some() {
                return Ok(Some(self.take_line()));
            }
        }
    }

    fn take_line(&mut self) -> String {
        let mut b = std::mem::take(&mut self.buf);
        if b.last() == Some(&b'\r') {
            b.pop();
        }
        match String::from_utf8(b) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        }
    }
}

/// Read `r` to its end keeping at most `cap` bytes (the rest is read and
/// dropped, so the child never blocks on a full pipe).
pub async fn read_capped<R: AsyncRead + Unpin>(mut r: R, cap: usize) -> String {
    let mut kept = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut dropped = false;
    loop {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
                dropped |= n > room;
            }
        }
    }
    let mut s = String::from_utf8_lossy(&kept).into_owned();
    if dropped {
        s.push_str("\n[… stderr truncated]");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_are_split_and_capped() {
        let data: &[u8] = b"one\r\ntwo\nthree";
        let mut l = BoundedLines::with_limits(tokio::io::BufReader::new(data), 16, 1024);
        assert_eq!(l.next_line().await.unwrap().as_deref(), Some("one"));
        assert_eq!(l.next_line().await.unwrap().as_deref(), Some("two"));
        assert_eq!(l.next_line().await.unwrap().as_deref(), Some("three"));
        assert!(l.next_line().await.unwrap().is_none());
        let long = [b'x'; 100];
        let mut l = BoundedLines::with_limits(tokio::io::BufReader::with_capacity(8, &long[..]), 16, 1024);
        assert!(matches!(l.next_line().await, Err(LineError::LineTooLong(16))));
        let many = b"ab\n".repeat(100);
        let mut l = BoundedLines::with_limits(tokio::io::BufReader::new(&many[..]), 16, 30);
        let mut n = 0;
        let err = loop {
            match l.next_line().await {
                Ok(Some(_)) => n += 1,
                Ok(None) => panic!("no total cap"),
                Err(e) => break e,
            }
        };
        assert!(matches!(err, LineError::TooMuchOutput(30)));
        assert_eq!(n, 10);
    }

    #[tokio::test]
    async fn stderr_is_capped_and_drained() {
        let big = [b'e'; 10_000];
        let s = read_capped(&big[..], 100).await;
        assert!(s.starts_with(&"e".repeat(100)) && s.contains("truncated"));
    }
}
