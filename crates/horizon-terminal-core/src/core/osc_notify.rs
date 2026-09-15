//! Streaming extraction of desktop-notification OSC sequences from the
//! raw PTY byte stream, ahead of `alacritty_terminal`'s parser.
//!
//! Terminal apps post notifications two ways:
//!
//! - OSC 9 (iTerm2's Growl-style hook): `ESC ] 9 ; <body> (BEL | ESC \)`
//! - OSC 777 `notify` (rxvt-unicode's): `ESC ] 777 ; notify ; <title> ; <body> (BEL | ESC \)`
//!
//! `alacritty_terminal` recognizes neither code, so left in the stream
//! both are swallowed silently — unknown OSC payloads never reach the grid
//! (good: nothing misrenders) but they also never produce an `Event`,
//! which is what this crate's normal observation path,
//! `core::events::EventSink`, is made of. The scanner below therefore runs
//! in front of `Processor::advance` in [`TerminalCore::write_vt`]: it
//! removes exactly the notification sequences from the byte stream (every
//! other byte passes through untouched, byte-for-byte, so title/clipboard/
//! color handling and grid rendering see what they always saw) and hands
//! the parsed [`TerminalNotification`]s back alongside the parser's own
//! events.
//!
//! The scan is a real state machine, not a byte search, because the PTY is
//! a byte stream, not a message stream: a sequence can be split across two
//! `write_vt` calls at any offset (`read_pty` cuts on 64 KiB read
//! boundaries, and a writer can flush mid-sequence). The scanner keeps the
//! still-incomplete tail across calls — both in pass-through state (an
//! `ESC ]` prefix not yet long enough to classify) and while capturing a
//! payload (until BEL or ST arrives).
//!
//! Deliberate limits:
//!
//! - Any OSC 777 is swallowed (alacritty would drop it either way), but
//!   only payloads whose first token is `notify` (case-insensitively,
//!   rxvt's spelling) become notifications.
//! - Any OSC 9 payload is taken as the body wholesale. ConEmu's
//!   Windows-only OSC 9 subcommands (`9;4;<state>` progress and friends)
//!   are not discriminated — such a payload would arrive as an odd-looking
//!   body; nothing misrenders either way, since the payload never reaches
//!   the grid.
//! - A payload that outgrows [`PAYLOAD_CAP`] before its terminator shows
//!   up is emitted truncated once and the remainder is swallowed until a
//!   terminator eventually arrives — bounding memory on a hostile or
//!   buggy stream that never terminates it.
//! - An `ESC` inside a payload that is *not* followed by `\` is treated as
//!   payload content (the scan keeps looking for BEL/ST): a malformed
//!   real-world sequence stays one notification instead of turning into a
//!   resync puzzle.

use crate::contract::TerminalNotification;

/// Cap on one notification payload's captured length, in bytes. A
/// notification is a sentence or three; 64 KiB is orders of magnitude
/// beyond every real sender (mirroring `OSC52_CLIPBOARD_WRITE_CAP`'s
/// "comfortably covers any legitimate use" reasoning) while keeping the
/// hold-back buffer bounded no matter what the PTY streams.
const PAYLOAD_CAP: usize = 64 * 1024;

/// Which notification OSC is being captured — only matters at parse time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureKind {
    /// OSC 9: the whole payload is the body.
    Nine,
    /// OSC 777: `notify ; <title> ; <body>`, parsed at the terminator.
    SevenSeven7,
}

/// Why [`OscNotificationScanner::scan_pass`] stopped.
enum PassOutcome {
    /// Nothing more classifiable in the buffer; consume through the offset
    /// and wait for the next chunk.
    NeedMore(usize),
    /// A notification OSC header was recognized; the capture is armed and
    /// the payload starts at the offset.
    Capture(usize),
}

/// Why [`OscNotificationScanner::scan_capture`] stopped.
enum CaptureOutcome {
    /// No terminator yet; consume through the offset and wait for the next
    /// chunk. While a capture is open the offset is always the payload
    /// start (undelivered payload bytes must not be consumed — they are
    /// only parsed once the terminator arrives), except once the capture
    /// is [`PAYLOAD_CAP`]-overflowing, where the payload is already emitted
    /// and disposable.
    NeedMore(usize),
    /// The sequence terminated; consume through the offset (past the
    /// terminator).
    Done(usize),
}

#[derive(Debug, Default)]
pub(super) struct OscNotificationScanner {
    /// Bytes held back from the previous [`Self::feed`] because they ended
    /// inside a not-yet-classifiable prefix (pass-through state) or inside
    /// a still-open capture. Everything before the resume point has been
    /// consumed and dispatched.
    buf: Vec<u8>,
    /// The capture currently in progress, if any.
    capture: Option<CaptureKind>,
    /// Offset in `buf` where the capture's payload starts (the byte after
    /// the `9 ;` / `777 ;` header). Only meaningful while `capture` is
    /// `Some` and the capture is not overflowing.
    payload_start: usize,
    /// Set once a capture has outgrown [`PAYLOAD_CAP`] with no terminator
    /// in sight: its (truncated) notification has been emitted and
    /// everything up to the terminator is now swallowed without buffering
    /// more than the trailing byte `ESC \` detection needs.
    overflowing: bool,
}

impl OscNotificationScanner {
    /// Feed one PTY chunk. Returns the bytes to hand to the VT parser (all
    /// non-notification content, in order) and the notifications completed
    /// by this chunk, in arrival order.
    pub(super) fn feed(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<TerminalNotification>) {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut notifications = Vec::new();
        let mut pos = 0;
        loop {
            match self.capture {
                None => match self.scan_pass(pos, &mut out) {
                    PassOutcome::NeedMore(next) => {
                        pos = next;
                        break;
                    }
                    PassOutcome::Capture(next) => pos = next,
                },
                Some(kind) => match self.scan_capture(pos, kind, &mut notifications) {
                    CaptureOutcome::NeedMore(next) => {
                        pos = next;
                        break;
                    }
                    CaptureOutcome::Done(next) => {
                        pos = next;
                        self.capture = None;
                    }
                },
            }
        }
        // Drop the consumed prefix, rebasing the payload offset with it —
        // after this, a still-open capture's payload always starts at
        // offset 0, so the next feed rescans from there.
        if self.capture.is_some() {
            self.payload_start = self.payload_start.saturating_sub(pos);
        }
        self.buf.drain(..pos);
        // An overflowing capture is dead weight by definition (its
        // notification already went out truncated): keep only the trailing
        // byte, the one cross-chunk `ESC \` detection still needs.
        if self.overflowing && self.buf.len() > 1 {
            let tail = self.buf.split_off(self.buf.len() - 1);
            self.buf = tail;
        }
        (out, notifications)
    }

    /// Pass-through scan from `pos`: copy every byte that is not part of a
    /// notification OSC to `out`, and either arm a capture when a
    /// notification header is recognized or hold back an incomplete
    /// prefix. Everything else — CSI sequences, other OSCs (title,
    /// clipboard, colors), stray ESCs — passes through byte-for-byte; the
    /// parser downstream owns it exactly as before this scanner existed.
    fn scan_pass(&mut self, mut pos: usize, out: &mut Vec<u8>) -> PassOutcome {
        loop {
            let buf = &self.buf;
            let Some(rel) = memchr(0x1b, &buf[pos..]) else {
                out.extend_from_slice(&buf[pos..]);
                return PassOutcome::NeedMore(buf.len());
            };
            let esc = pos + rel;
            out.extend_from_slice(&buf[pos..esc]);
            let Some(&after_esc) = buf.get(esc + 1) else {
                // The chunk ends mid-introducer; hold from the ESC. (Only
                // the ESC is held back — the next feed reclassifies.)
                return PassOutcome::NeedMore(esc);
            };
            if after_esc != b']' {
                // Not an OSC introducer. Pass the ESC through and rescan
                // from the byte after it — that byte may itself be an ESC
                // (`ESC ESC ] 9 ; …` chains), which must be classified on
                // its own, never swallowed as "some byte".
                out.push(0x1b);
                pos = esc + 1;
                continue;
            }
            // `ESC ]` — classify the OSC by its parameter number.
            let mut num: u32 = 0;
            let mut digits = 0;
            let mut i = esc + 2;
            while let Some(&b) = buf.get(i) {
                if !b.is_ascii_digit() {
                    break;
                }
                num = num.saturating_mul(10).saturating_add((b - b'0') as u32);
                digits += 1;
                i += 1;
            }
            let Some(&next) = buf.get(i) else {
                // The parameter number is still streaming in; hold from the
                // ESC — `ESC ] 7` must not be guessed to be OSC 7 before
                // its next byte arrives.
                return PassOutcome::NeedMore(esc);
            };
            let kind = match (digits, next) {
                (d, b';') if d > 0 && num == 9 => Some(CaptureKind::Nine),
                (d, b';') if d > 0 && num == 777 => Some(CaptureKind::SevenSeven7),
                _ => None,
            };
            match kind {
                Some(kind) => {
                    self.capture = Some(kind);
                    self.payload_start = i + 1;
                    self.overflowing = false;
                    return PassOutcome::Capture(i + 1);
                }
                None => {
                    // Any other OSC — title, palette, clipboard, or an
                    // empty/zero-parameter one — passes through whole
                    // (introducer through this byte) and stays the parser's
                    // business.
                    out.extend_from_slice(&buf[esc..=i]);
                    pos = i + 1;
                }
            }
        }
    }

    /// Capture scan from `pos` (which is `>= payload_start`): find the
    /// next BEL or ST, parse the payload accumulated since
    /// `payload_start`, and resume pass-through scanning after the
    /// terminator. A payload over [`PAYLOAD_CAP`] with no terminator in
    /// sight is emitted truncated once, then swallowed until a terminator
    /// eventually arrives.
    fn scan_capture(
        &mut self,
        pos: usize,
        kind: CaptureKind,
        notes: &mut Vec<TerminalNotification>,
    ) -> CaptureOutcome {
        let start = self.payload_start;
        let mut pos = pos.max(start);
        loop {
            let buf = &self.buf;
            let stop = match (memchr(0x07, &buf[pos..]), memchr(0x1b, &buf[pos..])) {
                (Some(bell), Some(esc)) => pos + bell.min(esc),
                (Some(bell), None) => pos + bell,
                (None, Some(esc)) => pos + esc,
                (None, None) => {
                    if !self.overflowing && buf.len() - start > PAYLOAD_CAP {
                        // No terminator and already over the cap: emit the
                        // truncated notification now — there may never be a
                        // terminator — and swallow the rest until one shows
                        // up.
                        self.overflowing = true;
                        notes.extend(parse_payload(kind, &buf[start..start + PAYLOAD_CAP]));
                        return CaptureOutcome::NeedMore(buf.len());
                    }
                    if self.overflowing {
                        // Nothing left to preserve; keep scanning from the
                        // buffer end (the feed tail-trims to one byte).
                        return CaptureOutcome::NeedMore(buf.len());
                    }
                    // Retain from the payload start: everything scanned so
                    // far is undelivered payload — it is only parsed once
                    // the terminator arrives, never consumed as output.
                    return CaptureOutcome::NeedMore(start);
                }
            };
            if buf[stop] == 0x07 {
                if !self.overflowing {
                    notes.extend(parse_payload(kind, &buf[start..stop]));
                }
                return CaptureOutcome::Done(stop + 1);
            }
            // An ESC inside the capture: `ESC \` (ST) terminates; anything
            // else is payload content and the scan continues past it.
            match buf.get(stop + 1) {
                None => {
                    return CaptureOutcome::NeedMore(if self.overflowing {
                        buf.len()
                    } else {
                        start
                    });
                }
                Some(b'\\') => {
                    if !self.overflowing {
                        notes.extend(parse_payload(kind, &buf[start..stop]));
                    }
                    return CaptureOutcome::Done(stop + 2);
                }
                Some(_) => pos = stop + 2,
            }
        }
    }
}

/// Parse a completed capture payload into the wire type. Clamped at
/// [`PAYLOAD_CAP`] (the cap is the only defense against a hostile payload;
/// a legitimate notification never gets near it), lossy on UTF-8 (a
/// multibyte character may have been split across PTY chunks — better a
/// replacement character than a dropped notification), and trimmed at the
/// edges. Emits `None` for payloads that carry nothing to show.
fn parse_payload(kind: CaptureKind, payload: &[u8]) -> Option<TerminalNotification> {
    let payload = &payload[..payload.len().min(PAYLOAD_CAP)];
    let text = String::from_utf8_lossy(payload);
    let text = text.trim();
    match kind {
        CaptureKind::Nine => {
            if text.is_empty() {
                return None;
            }
            Some(TerminalNotification {
                title: None,
                body: text.to_string(),
            })
        }
        CaptureKind::SevenSeven7 => {
            let mut parts = text.splitn(3, ';');
            let command = parts.next().unwrap_or("").trim();
            if !command.eq_ignore_ascii_case("notify") {
                return None;
            }
            let (title, body) = match (parts.next(), parts.next()) {
                (Some(title), Some(body)) => (Some(title.trim()), body.trim()),
                (Some(body), None) => (None, body.trim()),
                (None, _) => return None,
            };
            if title.is_none() && body.is_empty() {
                return None;
            }
            Some(TerminalNotification {
                title: title.filter(|t| !t.is_empty()).map(str::to_string),
                body: body.to_string(),
            })
        }
    }
}

/// First index of `needle` in `hay`, or `None`. A linear scan over the
/// bounded buffers here (payload ≤ [`PAYLOAD_CAP`], PTY chunks ≤ 64 KiB)
/// is plenty; no need for a substring-search dependency.
fn memchr(needle: u8, hay: &[u8]) -> Option<usize> {
    hay.iter().position(|&b| b == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(
        scanner: &mut OscNotificationScanner,
        chunks: &[&[u8]],
    ) -> (Vec<u8>, Vec<TerminalNotification>) {
        let mut passthrough = Vec::new();
        let mut notes = Vec::new();
        for chunk in chunks {
            let (out, batch) = scanner.feed(chunk);
            passthrough.extend(out);
            notes.extend(batch);
        }
        (passthrough, notes)
    }

    fn body_only(body: &str) -> TerminalNotification {
        TerminalNotification {
            title: None,
            body: body.to_string(),
        }
    }

    fn titled(title: &str, body: &str) -> TerminalNotification {
        TerminalNotification {
            title: Some(title.to_string()),
            body: body.to_string(),
        }
    }

    #[test]
    fn osc9_with_bel_terminator_is_extracted_and_removed() {
        let mut scanner = OscNotificationScanner::default();
        let (out, notes) = scanner.feed(b"before\x1b]9;build done\x07after");
        assert_eq!(out, b"beforeafter");
        assert_eq!(notes, vec![body_only("build done")]);
    }

    #[test]
    fn osc9_with_st_terminator_is_extracted() {
        let mut scanner = OscNotificationScanner::default();
        let (out, notes) = scanner.feed(b"\x1b]9;hello\x1b\\world");
        assert_eq!(out, b"world");
        assert_eq!(notes, vec![body_only("hello")]);
    }

    #[test]
    fn osc777_notify_with_title_and_body() {
        let mut scanner = OscNotificationScanner::default();
        let (out, notes) = scanner.feed(b"\x1b]777;notify;Deploy;finished in 3s\x07!");
        assert_eq!(out, b"!");
        assert_eq!(notes, vec![titled("Deploy", "finished in 3s")]);
    }

    #[test]
    fn osc777_notify_body_only_has_no_title() {
        let mut scanner = OscNotificationScanner::default();
        let (_, notes) = scanner.feed(b"\x1b]777;notify;just a body\x07");
        assert_eq!(notes, vec![body_only("just a body")]);
    }

    #[test]
    fn osc777_non_notify_is_swallowed_without_a_notification() {
        let mut scanner = OscNotificationScanner::default();
        let (out, notes) = scanner.feed(b"a\x1b]777;other;stuff\x07b");
        assert_eq!(out, b"ab");
        assert!(notes.is_empty());
    }

    #[test]
    fn other_osc_codes_pass_through_untouched() {
        let mut scanner = OscNotificationScanner::default();
        let input = &b"a\x1b]0;my title\x07b\x1b]52;c;abc\x1b\\"[..];
        let (out, notes) = feed_all(&mut scanner, &[input]);
        assert_eq!(out, input);
        assert!(notes.is_empty());
    }

    #[test]
    fn csi_sequences_pass_through_untouched() {
        let mut scanner = OscNotificationScanner::default();
        let input = &b"\x1b[2J\x1b[31mred\x1b[0m"[..];
        let (out, notes) = feed_all(&mut scanner, &[input]);
        assert_eq!(out, input);
        assert!(notes.is_empty());
    }

    #[test]
    fn two_notifications_in_one_chunk() {
        let mut scanner = OscNotificationScanner::default();
        let (out, notes) = scanner.feed(b"\x1b]9;one\x07mid\x1b]777;notify;T;B\x1b\\tail");
        assert_eq!(out, b"midtail");
        assert_eq!(notes, vec![body_only("one"), titled("T", "B")]);
    }

    #[test]
    fn sequence_split_across_every_byte_offset_still_parses() {
        // Japanese test data is deliberate (AGENTS.md): the payload must
        // survive a chunk boundary landing inside a multibyte character.
        let seq: &[u8] = "\x1b]777;notify;Title;body with ; and 日本語\x07tail".as_bytes();
        for split in 0..seq.len() {
            let mut scanner = OscNotificationScanner::default();
            let (out, notes) = feed_all(&mut scanner, &[&seq[..split], &seq[split..]]);
            assert_eq!(
                notes,
                vec![titled("Title", "body with ; and 日本語")],
                "split at {split}"
            );
            assert_eq!(out, b"tail", "split at {split}");
        }
    }

    #[test]
    fn header_split_across_chunks_is_never_misclassified() {
        // `ESC ] 77` alone must not be taken for OSC 77 (or guessed to be
        // OSC 777), and `ESC ] 777 ;` completed by the second chunk must
        // be recognized as the notification header.
        let mut scanner = OscNotificationScanner::default();
        let (out, notes) = feed_all(&mut scanner, &[b"\x1b]77", b"7;notify;T;B\x07x"]);
        assert_eq!(out, b"x");
        assert_eq!(notes, vec![titled("T", "B")]);
    }

    #[test]
    fn empty_and_headerless_osc9_produce_nothing() {
        let mut scanner = OscNotificationScanner::default();
        // Empty body: dropped entirely (header still swallowed — the
        // parser would ignore OSC 9 anyway).
        let (out, notes) = scanner.feed(b"\x1b]9;\x07x");
        assert_eq!(out, b"x");
        assert!(notes.is_empty());
        // No `;` at all: not a notification OSC — passed through byte-exact
        // for the parser to own.
        let input = &b"\x1b]9\x07"[..];
        let (out, notes) = feed_all(&mut scanner, &[input]);
        assert_eq!(out, input);
        assert!(notes.is_empty());
    }

    #[test]
    fn oversized_unterminated_payload_is_truncated_once_and_swallowed() {
        let mut scanner = OscNotificationScanner::default();
        let mut chunk = Vec::new();
        chunk.extend_from_slice(b"\x1b]9;");
        chunk.extend(std::iter::repeat_n(b'x', 100_000));
        // The flood stays bounded: after the over-cap chunk the scanner
        // keeps at most one byte (for cross-chunk `ESC \` detection), and
        // the truncated notification went out exactly once.
        let (_, notes) = scanner.feed(&chunk);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].body.len(), PAYLOAD_CAP);
        assert!(scanner.buf.len() <= 1);
        // The remainder is swallowed until the terminator; no second
        // emission, and the bytes after it pass through.
        let (out, notes) = feed_all(&mut scanner, &[b"more", b"\x07after"]);
        assert_eq!(out, b"after");
        assert!(notes.is_empty());
    }

    #[test]
    fn oversized_terminated_payload_is_truncated_once() {
        let mut scanner = OscNotificationScanner::default();
        let mut chunk = Vec::new();
        chunk.extend_from_slice(b"\x1b]9;");
        chunk.extend(std::iter::repeat_n(b'x', 100_000));
        chunk.extend_from_slice(b"\x07after");
        let (out, notes) = scanner.feed(&chunk);
        assert_eq!(out, b"after");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].body.len(), PAYLOAD_CAP);
    }

    #[test]
    fn bare_esc_inside_payload_stays_payload() {
        // A malformed ESC not followed by `\` is payload content: the scan
        // keeps looking for BEL/ST instead of resyncing mid-notification.
        let mut scanner = OscNotificationScanner::default();
        let (out, notes) = scanner.feed(b"\x1b]9;weird\x1b[31mbody\x07x");
        assert_eq!(out, b"x");
        assert_eq!(notes, vec![body_only("weird\x1b[31mbody")]);
    }
}
