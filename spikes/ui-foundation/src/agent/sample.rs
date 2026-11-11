//! A made-up session to show in the view: a history of ended turns, and a
//! turn in progress that can be advanced a step at a time the way a live
//! session's updates advance it.

use std::rc::Rc;

use super::Agent;
use super::display::{
    Approval, Call, CallBody, CallStatus, ComposerMode, DiffKind, DiffLine, Label, Message,
    Receipt, Status, Transcript, Turn, TurnPart, TurnState, Unit,
};

pub struct Session {
    transcript: Transcript,
    next_key: usize,
}

fn call(verb: &str, target: &str, status: CallStatus, body: Option<CallBody>) -> Call {
    Call {
        verb: verb.to_string(),
        target: target.to_string(),
        status,
        body,
        approval: None,
    }
}

const ANSWER: &str = "`font_size()` は `src/ui/font.rs` の静的な値を読んでいて、\
ボードのビューは起動時に一度だけその値を取っていました。次の 2 点を直します。\n\n\
- ボードの行の高さを、描画のたびに `font_size()` から計算する\n\
- 文字サイズのコマンドが、ボードのビューにも再描画を伝える\n\n\
変更後に `cargo nextest run --workspace` を通します。";

const CLOSING: &str =
    "修正しました。ボードの一覧とスレッドの両方で、文字サイズの変更がすぐに反映されます。";

impl Session {
    /// A session with `turns` ended turns, each a request and its answer:
    /// text, a receipt of four calls, text.
    pub fn long(turns: usize) -> Self {
        let mut session = Self {
            transcript: Transcript {
                model: "fable-5.2".to_string(),
                status: Status::Idle,
                units: Vec::new(),
                tasks: Vec::new(),
                composer: ComposerMode::Idle,
            },
            next_key: 0,
        };
        for turn in 0..turns {
            session.push(Unit::User(Message {
                label: Label::You,
                text: format!("{turn} 番目の依頼: フォントサイズの設定がボードに適用されないのを修正してほしい"),
            }));
            let receipt = session.receipt(turn);
            session.push(Unit::Turn(Turn {
                state: TurnState::Done,
                parts: vec![
                    // Not all one height: a turn in four says more.
                    TurnPart::Text(ANSWER.to_string() + &"\n\n補足の段落です。".repeat(turn % 4)),
                    TurnPart::Receipt(receipt),
                    TurnPart::Text(CLOSING.to_string()),
                ],
                elapsed: Some(format!("{}s", 20 + turn % 40)),
            }));
        }
        session
    }

    fn push(&mut self, unit: Unit) {
        self.transcript.units.push(Rc::new(unit));
    }

    fn receipt(&mut self, turn: usize) -> Receipt {
        let key = self.next_key;
        self.next_key += 1;
        let diff = CallBody::Diff(
            [
                (DiffKind::Context, "pub(crate) fn row_height() -> Pixels {"),
                (DiffKind::Removed, "    ROW_HEIGHT"),
                (DiffKind::Added, "    let size = font_size();"),
                (DiffKind::Added, "    px(size * 1.6)"),
                (DiffKind::Context, "}"),
            ]
            .into_iter()
            .map(|(kind, text)| DiffLine {
                kind,
                text: text.to_string(),
            })
            .collect(),
        );
        let output = CallBody::Output {
            lines: (0..12)
                .map(|line| {
                    format!("        PASS [   0.0{line}s] horizon board::tests::case_{turn}_{line}")
                })
                .collect(),
            omitted: 240,
        };
        Receipt {
            key,
            summary: "Read 1 file · 1 search · Edited 1 file · Ran 1 command".to_string(),
            running: false,
            failed: false,
            calls: vec![
                call("Read", "src/board/list.rs", CallStatus::Done, None),
                call("Search", "font_size", CallStatus::Done, None),
                call("Edit", "src/board/spec.rs", CallStatus::Done, Some(diff)),
                call(
                    "Run",
                    "cargo nextest run --workspace --locked",
                    CallStatus::Done,
                    Some(output),
                ),
            ],
        }
    }

    /// How many units the session has.
    pub fn units(&self) -> usize {
        self.transcript.units.len()
    }

    /// A request, and the turn answering it, thinking.
    pub fn begin_turn(&mut self, request: &str) {
        self.push(Unit::User(Message {
            label: Label::You,
            text: request.to_string(),
        }));
        self.push(Unit::Turn(Turn {
            state: TurnState::Running,
            parts: vec![TurnPart::Thinking],
            elapsed: None,
        }));
        self.transcript.status = Status::Running;
        self.transcript.composer = ComposerMode::Running;
    }

    /// Changes the turn in progress, as a new allocation: what an
    /// adaptation of a live session's frame hands the view.
    fn advance(&mut self, change: impl FnOnce(&mut Turn)) {
        let last = self.transcript.units.last_mut().expect("a turn");
        let Unit::Turn(turn) = last.as_ref() else {
            panic!("the last unit is not a turn");
        };
        let mut turn = turn.clone();
        change(&mut turn);
        *last = Rc::new(Unit::Turn(turn));
    }

    /// More of the turn's text arrives.
    pub fn stream(&mut self, more: &str) {
        self.advance(|turn| {
            turn.parts.retain(|part| *part != TurnPart::Thinking);
            match turn.parts.last_mut() {
                Some(TurnPart::Text(text)) => text.push_str(more),
                _ => turn.parts.push(TurnPart::Text(more.to_string())),
            }
        });
    }

    /// The turn starts a call; with `approval`, one that waits on the
    /// owner for that reason.
    pub fn call(&mut self, verb: &str, target: &str, approval: Option<&str>) {
        let key = self.next_key;
        self.next_key += 1;
        self.advance(|turn| {
            turn.parts.retain(|part| *part != TurnPart::Thinking);
            let mut call = call(verb, target, CallStatus::Running, None);
            call.approval = approval.map(|reason| Approval {
                id: format!("approval-{key}"),
                reason: reason.to_string(),
            });
            turn.parts.push(TurnPart::Receipt(Receipt {
                key,
                summary: format!("{verb} {target}"),
                running: true,
                failed: false,
                calls: vec![call],
            }));
        });
        if approval.is_some() {
            self.transcript.status = Status::WaitingForApproval;
            self.transcript.composer = ComposerMode::Approval {
                id: format!("approval-{key}"),
            };
        } else {
            self.transcript.status = Status::ToolRunning;
        }
    }

    /// The call in flight ends.
    pub fn finish_call(&mut self) {
        self.advance(|turn| {
            if let Some(TurnPart::Receipt(receipt)) = turn.parts.last_mut() {
                receipt.running = false;
                for call in &mut receipt.calls {
                    call.status = CallStatus::Done;
                    call.approval = None;
                }
            }
        });
        self.transcript.status = Status::Running;
        self.transcript.composer = ComposerMode::Running;
    }

    /// The turn ends.
    pub fn end_turn(&mut self) {
        self.advance(|turn| {
            turn.parts.retain(|part| *part != TurnPart::Thinking);
            turn.state = TurnState::Done;
            turn.elapsed = Some("12s".to_string());
        });
        self.transcript.status = Status::Idle;
        self.transcript.composer = ComposerMode::Idle;
    }

    /// The receipt keys of the unit at `index`, a turn.
    pub fn receipt_keys(&self, index: usize) -> Vec<usize> {
        match self.transcript.units[index].as_ref() {
            Unit::Turn(turn) => (turn.parts.iter())
                .filter_map(|part| match part {
                    TurnPart::Receipt(receipt) => Some(receipt.key),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Hands the view the session as it is now.
    pub fn show(&self, agent: &Agent) {
        agent.set_transcript(self.transcript.clone());
    }
}
