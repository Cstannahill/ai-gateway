// ─── Captured process output ─────────────────────────────────────────────────
//
// A bounded ring buffer of stdout/stderr lines with monotonically increasing
// sequence numbers, so a UI can tail it with a cursor (`after=<seq>`) instead
// of re-downloading everything. Sequence numbers keep increasing across
// restarts of the managed process, so a stale cursor can never re-show or skip
// lines from a different run.

use std::collections::VecDeque;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// Lines longer than this are truncated (llama-server can print very long
/// single lines, e.g. tokenizer dumps).
const MAX_LINE_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    /// `stdout`, `stderr`, or `gateway` (markers written by the manager).
    pub stream: &'static str,
    pub text: String,
}

/// One page of log output.
#[derive(Debug, Clone, Serialize)]
pub struct LogPage {
    pub lines: Vec<LogLine>,
    /// Pass back as `after` to get only newer lines. Equals the last line's
    /// `seq` (or the request's `after` when there was nothing new).
    pub next_cursor: u64,
    /// True when lines between `after` and the first returned line were
    /// already evicted from the ring buffer.
    pub truncated: bool,
}

struct Inner {
    lines: VecDeque<LogLine>,
    next_seq: u64,
}

pub struct LogBuffer {
    cap: usize,
    inner: Mutex<Inner>,
}

impl LogBuffer {
    pub fn new(cap: usize) -> Self {
        Self { cap: cap.max(1), inner: Mutex::new(Inner { lines: VecDeque::new(), next_seq: 1 }) }
    }

    pub fn push(&self, stream: &'static str, text: impl Into<String>) {
        let mut text: String = text.into();
        if text.len() > MAX_LINE_BYTES {
            let mut end = MAX_LINE_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str(" …[truncated]");
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let seq = inner.next_seq;
        inner.next_seq += 1;
        if inner.lines.len() >= self.cap {
            inner.lines.pop_front();
        }
        inner.lines.push_back(LogLine { seq, ts: Utc::now(), stream, text });
    }

    /// Lines with `seq > after` (all lines when `after` is `None`), oldest
    /// first, at most `limit`.
    pub fn read(&self, after: Option<u64>, limit: usize) -> LogPage {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let after_seq = after.unwrap_or(0);
        let first_available = inner.lines.front().map(|l| l.seq);
        let truncated = match (after, first_available) {
            (Some(a), Some(first)) => first > a + 1,
            _ => false,
        };
        let lines: Vec<LogLine> = inner
            .lines
            .iter()
            .filter(|l| l.seq > after_seq)
            .take(limit.max(1))
            .cloned()
            .collect();
        let next_cursor = lines.last().map(|l| l.seq).unwrap_or(after_seq);
        LogPage { lines, next_cursor, truncated }
    }

    /// The last `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<LogLine> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let skip = inner.lines.len().saturating_sub(n);
        inner.lines.iter().skip(skip).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_returns_only_newer_lines_in_order() {
        let b = LogBuffer::new(10);
        for i in 0..5 {
            b.push("stdout", format!("line {i}"));
        }
        let first = b.read(None, 100);
        assert_eq!(first.lines.len(), 5);
        assert_eq!(first.next_cursor, 5);

        b.push("stderr", "line 5");
        let next = b.read(Some(first.next_cursor), 100);
        assert_eq!(next.lines.len(), 1);
        assert_eq!(next.lines[0].text, "line 5");
        assert_eq!(next.lines[0].stream, "stderr");

        let none = b.read(Some(next.next_cursor), 100);
        assert!(none.lines.is_empty());
        assert_eq!(none.next_cursor, next.next_cursor, "cursor is stable when nothing is new");
    }

    #[test]
    fn ring_evicts_oldest_and_reports_truncation() {
        let b = LogBuffer::new(3);
        for i in 0..6 {
            b.push("stdout", format!("l{i}"));
        }
        let page = b.read(Some(1), 100);
        assert_eq!(page.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["l3", "l4", "l5"]);
        assert!(page.truncated, "lines 2..3 were evicted between the cursor and the first line");
        assert!(!b.read(Some(3), 100).truncated, "cursor at the eviction edge lost nothing");
        assert!(!b.read(None, 100).truncated);
    }

    #[test]
    fn limit_pages_forward_without_skipping() {
        let b = LogBuffer::new(100);
        for i in 0..7 {
            b.push("stdout", format!("l{i}"));
        }
        let p1 = b.read(None, 3);
        let p2 = b.read(Some(p1.next_cursor), 3);
        let p3 = b.read(Some(p2.next_cursor), 3);
        let all: Vec<_> = [p1, p2, p3].iter().flat_map(|p| p.lines.iter().map(|l| l.text.clone())).collect();
        assert_eq!(all, ["l0", "l1", "l2", "l3", "l4", "l5", "l6"]);
    }

    #[test]
    fn overlong_lines_are_truncated_on_a_char_boundary() {
        let b = LogBuffer::new(2);
        b.push("stdout", "é".repeat(MAX_LINE_BYTES));
        let line = &b.read(None, 1).lines[0];
        assert!(line.text.ends_with("[truncated]"));
        assert!(line.text.len() < MAX_LINE_BYTES + 32);
    }

    #[test]
    fn tail_returns_the_last_n_lines() {
        let b = LogBuffer::new(10);
        for i in 0..5 {
            b.push("stdout", format!("l{i}"));
        }
        assert_eq!(b.tail(2).iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["l3", "l4"]);
        assert_eq!(b.tail(99).len(), 5);
    }
}
