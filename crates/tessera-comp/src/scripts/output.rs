//! The last lines a script printed.

use std::collections::VecDeque;

use tessera_ipc::OutputLine;

/// Lines kept per script (design §7).
pub const MAX_LINES: usize = 500;
/// A line longer than this is split, so a program printing without newlines
/// cannot grow a buffer without bound.
const MAX_LINE_BYTES: usize = 4096;

/// Captured stdout and stderr, interleaved in the order they were read.
#[derive(Debug, Default)]
pub struct OutputBuffer {
    lines: VecDeque<OutputLine>,
    /// Bytes after the last newline, for stdout and stderr.
    partial: [Vec<u8>; 2],
}

impl OutputBuffer {
    /// Forgets everything, for a new run.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.partial = Default::default();
    }

    /// Adds bytes read from one of the pipes.
    pub fn push(&mut self, stderr: bool, bytes: &[u8]) {
        let slot = stderr as usize;
        for &byte in bytes {
            if byte == b'\n' {
                self.finish_line(stderr);
            } else {
                self.partial[slot].push(byte);
                if self.partial[slot].len() >= MAX_LINE_BYTES {
                    self.finish_line(stderr);
                }
            }
        }
    }

    /// Keeps a final line that had no newline, when the pipe closes.
    pub fn flush(&mut self, stderr: bool) {
        if !self.partial[stderr as usize].is_empty() {
            self.finish_line(stderr);
        }
    }

    /// The newest `count` lines, oldest first.
    pub fn tail(&self, count: usize) -> Vec<OutputLine> {
        let skip = self.lines.len().saturating_sub(count);
        self.lines.iter().skip(skip).cloned().collect()
    }

    fn finish_line(&mut self, stderr: bool) {
        let bytes = std::mem::take(&mut self.partial[stderr as usize]);
        let text = clean(&String::from_utf8_lossy(&bytes));
        if self.lines.len() == MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(OutputLine { text, stderr });
    }
}

/// Removes terminal colour codes and carriage returns, which the launcher
/// would otherwise draw as garbage.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => {
                // CSI: ESC [ parameters final-byte. Anything else: drop the ESC alone.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
            }
            '\r' => {}
            '\t' => out.push_str("    "),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(buffer: &OutputBuffer) -> Vec<String> {
        buffer
            .tail(MAX_LINES)
            .into_iter()
            .map(|line| line.text)
            .collect()
    }

    #[test]
    fn lines_split_on_newlines_across_reads() {
        let mut buffer = OutputBuffer::default();
        buffer.push(false, b"hel");
        buffer.push(false, b"lo\nwor");
        assert_eq!(texts(&buffer), ["hello"]);
        buffer.push(false, b"ld\n");
        assert_eq!(texts(&buffer), ["hello", "world"]);
    }

    #[test]
    fn stdout_and_stderr_keep_separate_partial_lines() {
        let mut buffer = OutputBuffer::default();
        buffer.push(false, b"out ");
        buffer.push(true, b"err\n");
        buffer.push(false, b"line\n");
        let lines = buffer.tail(10);
        assert_eq!(lines[0].text, "err");
        assert!(lines[0].stderr);
        assert_eq!(lines[1].text, "out line");
        assert!(!lines[1].stderr);
    }

    #[test]
    fn only_the_newest_lines_are_kept() {
        let mut buffer = OutputBuffer::default();
        for index in 0..MAX_LINES + 20 {
            buffer.push(false, format!("{index}\n").as_bytes());
        }
        let lines = texts(&buffer);
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[0], "20");
        assert_eq!(
            buffer.tail(2),
            buffer.tail(MAX_LINES)[MAX_LINES - 2..].to_vec()
        );
    }

    #[test]
    fn a_last_line_without_newline_is_kept_on_flush() {
        let mut buffer = OutputBuffer::default();
        buffer.push(false, b"no newline");
        assert!(texts(&buffer).is_empty());
        buffer.flush(false);
        assert_eq!(texts(&buffer), ["no newline"]);
        buffer.flush(false);
        assert_eq!(texts(&buffer).len(), 1, "flushing twice adds nothing");
    }

    #[test]
    fn colour_codes_and_carriage_returns_are_removed() {
        let mut buffer = OutputBuffer::default();
        buffer.push(false, b"\x1b[1;31mred\x1b[0m done\r\n");
        assert_eq!(texts(&buffer), ["red done"]);
    }

    #[test]
    fn endless_lines_are_split() {
        let mut buffer = OutputBuffer::default();
        buffer.push(false, &vec![b'x'; MAX_LINE_BYTES * 2 + 5]);
        buffer.flush(false);
        assert_eq!(texts(&buffer).len(), 3);
    }
}
