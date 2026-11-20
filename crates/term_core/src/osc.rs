//! Scanner for the shell-integration escape sequences that `alacritty_terminal` ignores:
//! OSC 7 (`file://host/path`, the shell's working directory) and OSC 133 (FinalTerm /
//! VS Code prompt marks: `A` prompt start, `B` command start, `C` executed, `D;code` finished).
//!
//! The scanner watches the raw byte stream (it never modifies it) and is fed every chunk read
//! from the PTY, so it keeps a tiny state machine across chunk boundaries. In the common case
//! (no `ESC` in the chunk) it is a single `memchr`-style pass.

use std::path::PathBuf;

/// Prompt marks emitted by the shell integration (OSC 133).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// `A`: the prompt is about to be drawn.
    PromptStart,
    /// `B`: the prompt ended, the user is typing a command.
    CommandStart,
    /// `C`: the command line was submitted and starts running.
    Executed,
    /// `D[;code]`: the command finished with this exit code (`None` if the shell gave none).
    Finished(Option<i32>),
}

/// Something the scanner found in the stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OscEvent {
    /// OSC 7: the shell's current directory.
    Cwd(PathBuf),
    /// OSC 133 mark.
    Mark(Mark),
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
/// Payloads longer than this are dropped (a path is at most a few hundred bytes).
const MAX_PAYLOAD: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// Saw `ESC`.
    Esc,
    /// Inside `ESC ]` ... collecting the payload.
    Osc,
    /// Saw `ESC` inside an OSC (probably the `ESC \` string terminator).
    OscEsc,
}

/// Incremental OSC 7 / OSC 133 recognizer.
#[derive(Debug, Default)]
pub(crate) struct OscScanner {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
}

impl OscScanner {
    /// Consumes the next chunk of the stream, calling `emit` for every complete sequence.
    pub(crate) fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(OscEvent)) {
        let mut i = 0;
        while i < bytes.len() {
            match self.state {
                State::Ground => match bytes[i..].iter().position(|&b| b == ESC) {
                    Some(p) => {
                        i += p + 1;
                        self.state = State::Esc;
                    },
                    None => return,
                },
                State::Esc => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        b']' => self.begin(),
                        ESC => {},
                        _ => self.state = State::Ground,
                    }
                },
                State::Osc => {
                    let rest = &bytes[i..];
                    match rest.iter().position(|&b| matches!(b, BEL | ESC | CAN | SUB)) {
                        None => {
                            self.push(rest);
                            return;
                        },
                        Some(p) => {
                            self.push(&rest[..p]);
                            let terminator = rest[p];
                            i += p + 1;
                            match terminator {
                                BEL => self.finish(&mut emit),
                                ESC => self.state = State::OscEsc,
                                // CAN / SUB abort the sequence.
                                _ => self.state = State::Ground,
                            }
                        },
                    }
                },
                State::OscEsc => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        b'\\' => self.finish(&mut emit),
                        b']' => self.begin(),
                        ESC => {},
                        _ => self.state = State::Ground,
                    }
                },
            }
        }
    }

    fn begin(&mut self) {
        self.buf.clear();
        self.overflow = false;
        self.state = State::Osc;
    }

    fn push(&mut self, data: &[u8]) {
        if self.overflow {
            return;
        }
        if self.buf.len() + data.len() > MAX_PAYLOAD {
            self.overflow = true;
            self.buf.clear();
        } else {
            self.buf.extend_from_slice(data);
        }
    }

    fn finish(&mut self, emit: &mut impl FnMut(OscEvent)) {
        self.state = State::Ground;
        if !self.overflow
            && let Ok(payload) = std::str::from_utf8(&self.buf)
            && let Some(event) = parse_payload(payload)
        {
            emit(event);
        }
        self.buf.clear();
    }
}

fn parse_payload(payload: &str) -> Option<OscEvent> {
    if let Some(uri) = payload.strip_prefix("7;") {
        return parse_osc7(uri).map(OscEvent::Cwd);
    }
    let rest = payload.strip_prefix("133;")?;
    let mut parts = rest.split(';');
    let mark = match parts.next()? {
        "A" => Mark::PromptStart,
        "B" => Mark::CommandStart,
        "C" => Mark::Executed,
        "D" => Mark::Finished(parts.next().and_then(|c| c.trim().parse().ok())),
        _ => return None,
    };
    Some(OscEvent::Mark(mark))
}

/// Parses the URI of an OSC 7 sequence into a local path.
///
/// Accepts `file://host/C:/Users/me`, `file:///C:/Users/me` and `file://server/share/dir`
/// (a UNC path when `server` is not this machine). Percent-escapes are decoded as UTF-8.
pub(crate) fn parse_osc7(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    let decoded = percent_decode(path)?;
    let bytes = decoded.as_bytes();
    let drive = bytes.len() >= 3 && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    if drive {
        // `/C:/Users/me` -> `C:\Users\me`
        let p = &decoded[1..];
        return Some(PathBuf::from(if cfg!(windows) { p.replace('/', "\\") } else { p.to_owned() }));
    }
    if !host.is_empty() && !is_local_host(host) && cfg!(windows) {
        return Some(PathBuf::from(format!("\\\\{host}{}", decoded.replace('/', "\\"))));
    }
    Some(PathBuf::from(decoded))
}

fn is_local_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || ["COMPUTERNAME", "HOSTNAME"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .any(|h| h.eq_ignore_ascii_case(host))
}

/// Decodes `%XX` escapes; invalid escapes are kept literally. `None` if the result is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<OscEvent> {
        let mut s = OscScanner::default();
        let mut out = Vec::new();
        for c in chunks {
            s.feed(c, |e| out.push(e));
        }
        out
    }

    #[test]
    fn marks_with_both_terminators() {
        let ev = scan(&[b"x\x1b]133;A\x07abc\x1b]133;B\x1b\\\x1b]133;C\x07\x1b]133;D;7\x07\x1b]133;D\x07"]);
        assert_eq!(
            ev,
            vec![
                OscEvent::Mark(Mark::PromptStart),
                OscEvent::Mark(Mark::CommandStart),
                OscEvent::Mark(Mark::Executed),
                OscEvent::Mark(Mark::Finished(Some(7))),
                OscEvent::Mark(Mark::Finished(None)),
            ]
        );
    }

    #[test]
    fn sequences_split_across_chunks() {
        let ev = scan(&[b"\x1b", b"]13", b"3;D;", b"42", b"\x1b", b"\\ tail \x1b[31mred"]);
        assert_eq!(ev, vec![OscEvent::Mark(Mark::Finished(Some(42)))]);
    }

    #[cfg(windows)]
    #[test]
    fn osc7_windows_paths() {
        let ev = scan(&[b"\x1b]7;file://PC/C:/Users/venta/My%20Dir/%C3%B1\x07"]);
        assert_eq!(ev, vec![OscEvent::Cwd(PathBuf::from("C:\\Users\\venta\\My Dir\\\u{f1}"))]);
        assert_eq!(parse_osc7("file:///D:/x"), Some(PathBuf::from("D:\\x")));
        assert_eq!(parse_osc7("file://server/share/dir"), Some(PathBuf::from("\\\\server\\share\\dir")));
        assert_eq!(parse_osc7("file://localhost/C%3A/tmp"), Some(PathBuf::from("C:\\tmp")));
    }

    #[test]
    fn malformed_input_is_ignored() {
        assert!(scan(&[b"\x1b]7;http://x/y\x07\x1b]133;Z\x07\x1b]0;title\x07"]).is_empty());
        assert_eq!(parse_osc7("file://host"), None);
        // A broken escape must not swallow the next valid sequence.
        let ev = scan(&[b"\x1b]7;file:///C:/a\x1b]133;A\x07"]);
        assert_eq!(ev, vec![OscEvent::Mark(Mark::PromptStart)]);
    }

    #[test]
    fn oversized_payload_is_dropped_and_scanner_recovers() {
        let mut big = b"\x1b]7;file:///C:/".to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX_PAYLOAD + 10));
        big.push(BEL);
        big.extend_from_slice(b"\x1b]133;A\x07");
        assert_eq!(scan(&[&big]), vec![OscEvent::Mark(Mark::PromptStart)]);
    }

    #[test]
    fn bad_percent_escapes_stay_literal() {
        assert_eq!(percent_decode("/a%zz%4"), Some("/a%zz%4".to_string()));
        assert_eq!(percent_decode("/%FF"), None);
    }
}
