//! A measurement, run by hand: a real program in a PTY of its own, the
//! terminal core reading what it writes, and a log of what the program
//! reports about itself over OSC 7501 and when. Nothing here is asserted,
//! and the test is ignored: what a program reports is the program's to
//! change, and running one costs whoever runs it.
//!
//! ```sh
//! HORIZON_PROBE_SCRIPT=steps.txt HORIZON_PROBE_LOG=probe.log \
//!   cargo nextest run -p horizon-terminal-core --run-ignored only \
//!   what_a_program_reports
//! ```
//!
//! `HORIZON_PROBE_COMMAND` is the program and its arguments (`claude` when
//! unset), `HORIZON_PROBE_CWD` where it runs, `HORIZON_PROBE_ANSWER=0` has
//! the terminal leave the program's question unanswered. The script is one
//! step a line:
//!
//! ```text
//! wait 2000            # milliseconds
//! type list the files  # the rest of the line, as typed
//! enter                # also: esc, ctrl-c, ctrl-d, tab, down, up
//! hex 7f               # those bytes
//! until done 60000     # a report of that state (or `clear`), or the time
//! screen               # what the terminal shows, into the log
//! ```

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::TerminalSize;
use crate::core::TerminalCore;

/// The prefix of an OSC 7501 sequence, for the log of what arrived whole.
const PREFIX: &[u8] = b"\x1b]7501;";

struct Probe {
    core: TerminalCore,
    chunks: crossbeam_channel::Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    log: Box<dyn Write>,
    started: Instant,
    /// The tail of the stream not yet searched for whole sequences.
    raw: Vec<u8>,
    /// The states reported since the last `until`, as the protocol spells
    /// them.
    seen: Vec<String>,
    /// Whether the terminal answers the program's question.
    answers: bool,
    ended: bool,
}

impl Probe {
    fn note(&mut self, what: &str) {
        let at = self.started.elapsed().as_secs_f64();
        let _ = writeln!(self.log, "[{at:9.3}s] {what}");
        let _ = self.log.flush();
    }

    /// Takes in what the program wrote for `time`, answering what it asks
    /// the terminal as a terminal does.
    fn pump(&mut self, time: Duration) {
        let until = Instant::now() + time;
        while !self.ended {
            let left = until.saturating_duration_since(Instant::now());
            match self.chunks.recv_timeout(left) {
                Ok(chunk) => self.take(&chunk),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => return,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    self.ended = true;
                    self.note("the program's output ended");
                }
            }
        }
    }

    fn take(&mut self, chunk: &[u8]) {
        self.raw.extend_from_slice(chunk);
        self.note_raw();
        let events = self.core.write_vt(chunk);
        for reply in events.pty_writes {
            if reply.starts_with(PREFIX) {
                if !self.answers {
                    self.note("asked; left unanswered");
                    continue;
                }
                self.note("asked; answered");
            }
            let _ = self.writer.write_all(&reply);
            let _ = self.writer.flush();
        }
        for update in events.program_status {
            self.note(&format!("read  {update:?}"));
        }
    }

    /// Logs every whole OSC 7501 sequence in the stream as it was sent,
    /// the ones the core drops among them.
    fn note_raw(&mut self) {
        loop {
            let Some(start) = find(&self.raw, PREFIX) else {
                // Keep what could still be the start of a sequence.
                let keep = self.raw.len().min(PREFIX.len() - 1);
                self.raw.drain(..self.raw.len() - keep);
                return;
            };
            let body = start + PREFIX.len();
            let end = self.raw[body..]
                .iter()
                .position(|&b| b == 0x07 || b == 0x1b)
                .map(|at| body + at);
            let Some(end) = end else {
                self.raw.drain(..start);
                return;
            };
            let text = String::from_utf8_lossy(&self.raw[body..end]).into_owned();
            if let Some(state) = text.split(':').find_map(|pair| pair.strip_prefix("state="))
                && !text.split(':').any(|pair| pair.starts_with("id="))
            {
                self.seen.push(state.to_string());
            }
            self.note(&format!("sent  {text}"));
            self.raw.drain(..end);
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    fn step(&mut self, line: &str) {
        let line = line.split(" #").next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') {
            return;
        }
        self.note(&format!("step  {line}"));
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        match verb {
            "wait" => self.pump(Duration::from_millis(rest.parse().expect("wait <ms>"))),
            "type" => self.send(rest.as_bytes()),
            "enter" => self.send(b"\r"),
            "esc" => self.send(b"\x1b"),
            "tab" => self.send(b"\t"),
            "up" => self.send(b"\x1b[A"),
            "down" => self.send(b"\x1b[B"),
            "ctrl-c" => self.send(b"\x03"),
            "ctrl-d" => self.send(b"\x04"),
            "hex" => {
                let bytes: Vec<u8> = (rest.split_whitespace())
                    .map(|byte| u8::from_str_radix(byte, 16).expect("hex <byte>.."))
                    .collect();
                self.send(&bytes);
            }
            "screen" => {
                let text = self.core.snapshot_text();
                let shown: Vec<&str> = text
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .collect();
                self.note(&format!("screen\n{}", shown.join("\n")));
            }
            "until" => {
                let (state, time) = rest.split_once(' ').expect("until <state> <ms>");
                let until = Instant::now() + Duration::from_millis(time.parse().expect("ms"));
                self.seen.clear();
                while !self.seen.iter().any(|seen| seen == state) {
                    if self.ended || Instant::now() >= until {
                        self.note(&format!("no report of {state} in time"));
                        return;
                    }
                    self.pump(Duration::from_millis(50));
                }
            }
            other => panic!("unknown step {other:?}"),
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len())
        .position(|window| window == needle)
}

#[test]
#[ignore = "runs a real program, by hand: see this file's top"]
fn what_a_program_reports() {
    let script = std::env::var("HORIZON_PROBE_SCRIPT").expect("HORIZON_PROBE_SCRIPT=<file>");
    let script = std::fs::read_to_string(script).expect("the script");
    let command = std::env::var("HORIZON_PROBE_COMMAND").unwrap_or_else(|_| "claude".into());
    let mut words = command.split_whitespace();
    let log: Box<dyn Write> = match std::env::var("HORIZON_PROBE_LOG") {
        Ok(path) => Box::new(std::fs::File::create(path).expect("the log")),
        Err(_) => Box::new(std::io::stderr()),
    };

    let (cols, rows) = (120, 40);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("a pty");
    let mut builder = CommandBuilder::new(words.next().expect("a program"));
    builder.args(words);
    // The program is to see a terminal of its own and nothing of the
    // session this test runs in: a program started from inside another of
    // its kind, or told it runs in the background, reports differently.
    for (name, _) in std::env::vars() {
        if name.starts_with("CLAUDE") || name.starts_with("HORIZON_") || name == "TMUX" {
            builder.env_remove(name);
        }
    }
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");
    if let Ok(cwd) = std::env::var("HORIZON_PROBE_CWD") {
        builder.cwd(cwd);
    }
    let mut child = pair.slave.spawn_command(builder).expect("the program");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("a reader");
    let (chunks_tx, chunks) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 64 * 1024];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 || chunks_tx.send(buffer[..read].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut probe = Probe {
        core: TerminalCore::new(TerminalSize::new(cols, rows)),
        chunks,
        writer: pair.master.take_writer().expect("a writer"),
        log,
        started: Instant::now(),
        raw: Vec::new(),
        seen: Vec::new(),
        answers: std::env::var("HORIZON_PROBE_ANSWER").as_deref() != Ok("0"),
        ended: false,
    };
    probe.note(&format!("started {command}"));
    for line in script.lines() {
        probe.step(line);
    }
    let _ = child.kill();
    probe.pump(Duration::from_millis(500));
    probe.note("end");
}
