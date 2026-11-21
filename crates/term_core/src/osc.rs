//! Scanner for the shell-integration escape sequences that `alacritty_terminal` ignores:
//! OSC 7 (`file://host/path`, the shell's working directory), OSC 133 (FinalTerm /
//! VS Code prompt marks: `A` prompt start, `B` command start, `C` executed, `D;code` finished)
//! and OSC 633 `E` (VS Code's "exact command line": `OSC 633 ; E ; <escaped command> ST`).
//!
//! OSC 633 `E` escaping (same as VS Code, with one extension for robustness): a backslash is
//! written `\\` and every byte that is a control character, `;`, or outside printable ASCII is
//! written `\xNN` (lowercase hex, one escape per UTF-8 byte). The integration script escapes
//! all non-ASCII bytes so the command survives any console output encoding.
//!
//! The scanner never modifies the stream. [`OscScanner::next_event`] reports where each sequence
//! ends so the caller can feed the terminal parser up to exactly that byte and read the cursor
//! position at the moment the sequence was emitted.

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
    /// OSC 633 `E`: the exact command line about to run (already unescaped).
    Command(String),
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
/// Payloads longer than this are dropped (a path or an escaped command line fits easily).
const MAX_PAYLOAD: usize = 16 * 1024;

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

/// Incremental OSC 7 / OSC 133 / OSC 633 recognizer.
#[derive(Debug, Default)]
pub(crate) struct OscScanner {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
}

impl OscScanner {
    /// Consumes the next chunk of the stream, calling `emit` for every complete sequence.
    #[cfg(test)]
    pub(crate) fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(OscEvent)) {
        let mut rest = bytes;
        while let Some((end, event)) = self.next_event(rest) {
            emit(event);
            rest = &rest[end..];
        }
    }

    /// Consumes bytes until the first complete sequence the scanner understands.
    ///
    /// Returns how many bytes were consumed (the sequence ends right there) and the event; the
    /// caller continues with `bytes[consumed..]`. `None` means the whole chunk was consumed
    /// without completing a sequence (the state is kept for the next chunk).
    pub(crate) fn next_event(&mut self, bytes: &[u8]) -> Option<(usize, OscEvent)> {
        let mut i = 0;
        while i < bytes.len() {
            match self.state {
                State::Ground => {
                    i += bytes[i..].iter().position(|&b| b == ESC)? + 1;
                    self.state = State::Esc;
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
                            return None;
                        },
                        Some(p) => {
                            self.push(&rest[..p]);
                            let terminator = rest[p];
                            i += p + 1;
                            match terminator {
                                BEL => {
                                    if let Some(event) = self.finish() {
                                        return Some((i, event));
                                    }
                                },
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
                        b'\\' => {
                            if let Some(event) = self.finish() {
                                return Some((i, event));
                            }
                        },
                        b']' => self.begin(),
                        ESC => {},
                        _ => self.state = State::Ground,
                    }
                },
            }
        }
        None
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

    fn finish(&mut self) -> Option<OscEvent> {
        self.state = State::Ground;
        let event = if self.overflow {
            None
        } else {
            // Lossy: a shell writing raw non-UTF-8 bytes must not lose the whole sequence.
            parse_payload(&String::from_utf8_lossy(&self.buf))
        };
        self.buf.clear();
        event
    }
}

fn parse_payload(payload: &str) -> Option<OscEvent> {
    if let Some(uri) = payload.strip_prefix("7;") {
        return parse_osc7(uri).map(OscEvent::Cwd);
    }
    if let Some(rest) = payload.strip_prefix("633;E") {
        // `633;E;<command>[;<nonce>]`; a `;` inside the command is escaped as `\x3b`.
        let rest = rest.strip_prefix(';').unwrap_or("");
        return Some(OscEvent::Command(unescape_command(rest.split(';').next().unwrap_or(""))));
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

/// Reverses the OSC 633 `E` escaping: `\\` is a backslash and `\xNN` a raw byte; the bytes are
/// then decoded as UTF-8 (lossily).
pub(crate) fn unescape_command(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            if b[i + 1] == b'\\' {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if b[i + 1] == b'x'
                && i + 3 < b.len()
                && let (Some(h), Some(l)) = (hex(b[i + 2]), hex(b[i + 3]))
            {
                out.push(h << 4 | l);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escapes a command line for OSC 633 `E` (the inverse of [`unescape_command`]; this is what
/// `integration.ps1` implements in PowerShell).
#[cfg(test)]
pub(crate) fn escape_command(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for &b in s.as_bytes() {
        if b == b'\\' {
            out.push_str("\\\\");
        } else if !(0x20..=0x7e).contains(&b) || b == b';' {
            let _ = write!(out, "\\x{b:02x}");
        } else {
            out.push(char::from(b));
        }
    }
    out
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

    #[test]
    fn next_event_reports_where_the_sequence_ends() {
        let mut s = OscScanner::default();
        let data = b"ab\x1b]133;A\x07cd\x1b]133;C\x07ef";
        let (end, ev) = s.next_event(data).unwrap();
        assert_eq!(ev, OscEvent::Mark(Mark::PromptStart));
        assert_eq!(&data[..end], b"ab\x1b]133;A\x07");
        let rest = &data[end..];
        let (end2, ev2) = s.next_event(rest).unwrap();
        assert_eq!(ev2, OscEvent::Mark(Mark::Executed));
        assert_eq!(&rest[..end2], b"cd\x1b]133;C\x07");
        assert!(s.next_event(&rest[end2..]).is_none());
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

    #[test]
    fn osc633_command_is_unescaped() {
        let ev = scan(&[b"\x1b]633;E;git commit -m \"a\\x3bb\" C:\\\\tmp\x07"]);
        assert_eq!(ev, vec![OscEvent::Command("git commit -m \"a;b\" C:\\tmp".to_string())]);
        // A nonce after the command is ignored; both terminators work.
        let ev = scan(&[b"\x1b]633;E;ls;1234-nonce\x1b\\"]);
        assert_eq!(ev, vec![OscEvent::Command("ls".to_string())]);
        // Empty command.
        assert_eq!(scan(&[b"\x1b]633;E;\x07"]), vec![OscEvent::Command(String::new())]);
        assert_eq!(scan(&[b"\x1b]633;E\x07"]), vec![OscEvent::Command(String::new())]);
        // Other 633 subcommands are ignored.
        assert!(scan(&[b"\x1b]633;A\x07\x1b]633;P;Cwd=C:\\x\x07"]).is_empty());
    }

    #[test]
    fn command_escaping_roundtrips() {
        for cmd in [
            "echo hola; echo adios",
            "dir C:\\Users\\me",
            "echo \"ñandú\" 日本語 😀",
            "line1\nline2\ttab",
            "trailing\\",
            "a\\x41 literal",
            "",
        ] {
            let wire = format!("\x1b]633;E;{}\x07", escape_command(cmd));
            assert!(wire.bytes().all(|b| b == 0x1b || b == 0x07 || (0x20..0x7f).contains(&b)), "{wire:?}");
            assert_eq!(scan(&[wire.as_bytes()]), vec![OscEvent::Command(cmd.to_string())], "{cmd:?}");
        }
    }

    #[test]
    fn command_split_across_chunks() {
        let ev = scan(&[b"\x1b]633;E;echo ", b"hi\\x", b"3b x", b"\x07"]);
        assert_eq!(ev, vec![OscEvent::Command("echo hi; x".to_string())]);
    }
}
