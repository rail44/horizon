//! Horizon's agent pane on this stack: a scrolling column of the
//! conversation's units above a composer.
//!
//! What it draws is Horizon's own display model, used as it is
//! (`src/agent/view/display.rs`), and its fixed labels
//! (`src/agent/view/chrome.rs`). The column holds the units near what is
//! in view as elements of the document ([`crate::windowed`]), or, asked to,
//! all of them. A unit that is the same allocation as before keeps its
//! elements; the turn in progress is a new allocation on every update and
//! is built again.

use std::collections::HashSet;
use std::rc::Rc;

use blitz_traits::events::DomEventData;
use keyboard_types::{Key, NamedKey};
use reactive_graph::effect::Effect;
use reactive_graph::owner::{LocalStorage, StoredValue};
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, GetUntracked, Set, Update, WithValue};

use crate::windowed::{follow_tail, windowed};
use crate::{El, dispatched, el, prevent_default, stop_propagation};

#[allow(dead_code)]
#[path = "../../../src/agent/view/chrome.rs"]
mod chrome;
#[allow(dead_code)]
#[path = "../../../src/agent/view/display.rs"]
mod display;
pub mod sample;

use display::{
    Approval, Call, CallBody, CallStatus, ComposerMode, DiffKind, Label, Notice, Receipt, Status,
    Task, Tone, Transcript, Turn, TurnPart, TurnState, Unit,
};

const COMPOSER_MAX_ROWS: usize = 8;
const COMPOSER_LINE: usize = 22;

// Every rule sits under the view's own class: the page's style sheet is one
// for all the views on it.
pub const CSS: &str = "
    .agent { display: flex; flex-direction: column; height: 100%; min-width: 240px;
             outline: none; font-family: sans-serif; font-size: 14px; line-height: 22px;
             color: #202020; background: #fafafa;
        .column { flex: 1; min-height: 0; overflow-y: scroll; padding: 24px 24px 16px 24px; }
        .unit { padding-bottom: 8px; }
        .user { margin-left: 24px; padding: 8px 12px; border-radius: 4px; background: #efefef;
                white-space: pre-wrap; }
        .user.muted { color: #808080; }
        .turn { position: relative; display: flex; flex-direction: column; gap: 4px;
                padding: 8px 12px; border: 1px solid #d8d8d8; border-radius: 4px; }
        .bar { position: absolute; left: 0; top: 0; bottom: 0; width: 2px; background: #356df3; }
        .notice, .task { padding: 0 12px; font-size: 12px; line-height: 18px; color: #808080; }
        .footer { display: flex; flex-direction: column; gap: 8px; }
        .warning { color: #b7791f; }
        .danger { color: #c0392b; }
        .line { display: flex; align-items: center; gap: 8px; }
        .spacer { flex: 1; min-width: 0; }
        .chip { flex: none; padding: 0 4px; border-radius: 3px; font-size: 11px;
                line-height: 16px; background: #ececec; color: #606060; }
        .chip.accent { background: #e3ebfd; color: #2a56c6; }
        .chip.danger { background: #fbe9e7; color: #c0392b; }
        .button { flex: none; padding: 0 8px; border-radius: 4px; font-size: 12px;
                  line-height: 20px; color: #606060; cursor: pointer; }
        .button:hover { background: #ececec; }
        .button.primary { padding: 0 12px; line-height: 28px; background: #356df3; color: white; }
        .button.primary:hover { background: #477df8; }
        .receipt { display: flex; flex-direction: column; gap: 4px; }
        .receipt-line { display: flex; align-items: center; gap: 8px; cursor: pointer;
                        font-size: 12px; line-height: 18px; color: #808080; }
        .calls { display: flex; flex-direction: column; gap: 4px; margin-left: 8px;
                 padding-left: 12px; border-left: 1px solid #d8d8d8; }
        .call { display: flex; flex-direction: column; gap: 4px; }
        .call-row { display: flex; align-items: center; gap: 8px; font-size: 12px;
                    line-height: 18px; }
        .call-row.opens { cursor: pointer; }
        .dot-cell { flex: none; width: 16px; display: flex; align-items: center;
                    justify-content: center; }
        .dot { width: 8px; height: 8px; border-radius: 4px; background: #a0a0a0; }
        .dot.failed { background: #c0392b; }
        .dot.running { background: #356df3; }
        .dot.denied { background: #b7791f; }
        .verb { flex: none; color: #808080; }
        .target { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;
                  white-space: nowrap; font-family: monospace; }
        .mono { font-family: monospace; font-size: 12px; line-height: 18px; white-space: pre; }
        .diff-line { display: flex; color: #808080; }
        .diff-line.added { background: #e6f4ea; color: #1e7b34; }
        .diff-line.removed { background: #fbe9e7; color: #c0392b; }
        .mark { flex: none; width: 16px; }
        .output { overflow-x: scroll; color: #808080; }
        .more { font-size: 11px; line-height: 16px; color: #808080; }
        .approval { display: flex; align-items: center; gap: 8px; font-size: 12px;
                    line-height: 18px; }
        .reason { flex: 1; min-width: 0; }
        .end { display: flex; justify-content: flex-end; align-items: baseline; gap: 8px;
               font-size: 12px; line-height: 18px; color: #808080; }
        .thinking { display: flex; font-size: 11px; line-height: 16px; color: #808080; }
        .thinking .d { animation: thinking 1.2s linear infinite; }
        .thinking .d1 { animation-delay: -0.8s; }
        .thinking .d2 { animation-delay: -0.4s; }
        .composer { flex: none; padding: 0 24px 12px 24px; }
        .box { display: flex; flex-direction: column; gap: 4px; padding: 8px 12px;
               border: 1px solid #d8d8d8; border-radius: 4px; }
        textarea { font-family: sans-serif; font-size: 14px; line-height: 22px; border: none;
                   outline: none; padding: 0; margin: 0; background: transparent; resize: none; }
        textarea:focus { outline: none; }
        .hints { display: flex; align-items: center; gap: 4px; font-size: 11px;
                 line-height: 16px; color: #808080; }
        .model { cursor: pointer; }
        .text h1, .text h2, .text h3 { font-size: 14px; line-height: 22px; font-weight: 600;
                                       margin: 8px 0 4px 0; }
        .text p, .text ul, .text ol, .text pre, .text blockquote { margin: 0 0 8px 0; }
        .text .body > :first-child { margin-top: 0; }
        .text .body > :last-child { margin-bottom: 0; }
        .text pre { padding: 8px 12px; border-radius: 4px; background: #f0f0f0;
                    font-size: 13px; line-height: 20px; overflow-x: scroll; }
        .text code { font-family: monospace; font-size: 13px; }
        .link { color: #356df3; }
    }
    @keyframes thinking { from { opacity: 1; } to { opacity: 0.25; } }
";

/// What the view asks of whatever holds it.
#[derive(Clone, Debug, PartialEq)]
pub enum Asked {
    Send(String),
    Approve(String),
    Deny(String),
    Stop,
    SwitchModel,
}

/// The view's state: the session as last handed in, and what is the view's
/// own (which receipts and call bodies are open, what is typed).
#[derive(Clone, Copy)]
pub struct Agent {
    units: RwSignal<Vec<Rc<Unit>>, LocalStorage>,
    status: RwSignal<Status>,
    model: RwSignal<String>,
    tasks: RwSignal<Vec<Task>>,
    composer: RwSignal<ComposerMode>,
    /// Receipts opened by a click, by their key.
    pub expanded: RwSignal<HashSet<usize>>,
    /// Call bodies opened by a click, by receipt key and call index.
    pub open_bodies: RwSignal<HashSet<(usize, usize)>>,
    /// What the composer holds.
    pub typed: RwSignal<String>,
    /// Which units are elements, by their places in the conversation.
    pub built: RwSignal<(usize, usize)>,
    ask: StoredValue<Rc<dyn Fn(Asked)>, LocalStorage>,
    /// The column, and the composer's field: what takes the focus when the
    /// view is given it.
    pub column: El,
    pub field: El,
}

impl Agent {
    /// Replaces what the view shows. A unit equal to the one in its place
    /// stays the allocation it was, so its elements stay too.
    fn set_transcript(&self, transcript: Transcript) {
        let old = self.units.get_untracked();
        let units: Vec<Rc<Unit>> = (transcript.units.into_iter().enumerate())
            .map(|(index, unit)| match old.get(index) {
                Some(before) if Rc::ptr_eq(before, &unit) || *before == unit => before.clone(),
                _ => unit,
            })
            .collect();
        let same = old.len() == units.len()
            && old
                .iter()
                .zip(&units)
                .all(|(before, after)| Rc::ptr_eq(before, after));
        if !same {
            self.units.set(units);
        }
        if self.status.get_untracked() != transcript.status {
            self.status.set(transcript.status);
        }
        if self.model.get_untracked() != transcript.model {
            self.model.set(transcript.model);
        }
        if self.tasks.get_untracked() != transcript.tasks {
            self.tasks.set(transcript.tasks);
        }
        if self.composer.get_untracked() != transcript.composer {
            self.composer.set(transcript.composer);
        }
    }

    fn ask(&self, asked: Asked) {
        self.ask.with_value(|ask| ask(asked));
    }

    /// Enter in the composer: send what it holds, or, waiting on an
    /// approval with nothing typed, approve.
    fn submit(&self) {
        let text = self.typed.get_untracked().trim().to_string();
        if text.is_empty() {
            if let ComposerMode::Approval { id } = self.composer.get_untracked() {
                self.ask(Asked::Approve(id));
            }
            return;
        }
        self.typed.set(String::new());
        let field = self.field;
        field.set_value("");
        self.ask(Asked::Send(text));
    }

    /// Denies the pending approval when nothing is typed; whether it did.
    fn deny_pending(&self) -> bool {
        let ComposerMode::Approval { id } = self.composer.get_untracked() else {
            return false;
        };
        if !self.typed.get_untracked().trim().is_empty() {
            return false;
        }
        self.ask(Asked::Deny(id));
        true
    }
}

/// How the column holds the conversation.
#[derive(Clone, Copy, PartialEq)]
pub enum Column {
    /// Only the units near what is in view are elements.
    Windowed,
    /// Every unit is an element, in view or not.
    Whole,
}

/// Builds the view, empty; `ask` hears what the owner asks for.
pub fn view(ask: impl Fn(Asked) + 'static) -> (El, Agent) {
    view_with(ask, Column::Windowed)
}

pub fn view_with(ask: impl Fn(Asked) + 'static, column: Column) -> (El, Agent) {
    let state = Agent {
        units: RwSignal::new_local(Vec::new()),
        status: RwSignal::new(Status::Idle),
        model: RwSignal::new(String::new()),
        tasks: RwSignal::new(Vec::new()),
        composer: RwSignal::new(ComposerMode::Idle),
        expanded: RwSignal::new(HashSet::new()),
        open_bodies: RwSignal::new(HashSet::new()),
        typed: RwSignal::new(String::new()),
        built: RwSignal::new((0, 0)),
        ask: StoredValue::new_local(Rc::new(ask)),
        column: el("div").class("column"),
        field: el("textarea"),
    };

    let units = move || (state.units.get().into_iter().enumerate()).collect::<Vec<_>>();
    // A unit kept from the last update is the allocation it was.
    let key = |(index, unit): &(usize, Rc<Unit>)| (*index, Rc::as_ptr(unit) as usize);
    let unit = move |(_, unit): &(usize, Rc<Unit>)| {
        el("div").class("unit").child(match unit.as_ref() {
            Unit::User(message) => el("div")
                .class(match message.label {
                    Label::You => "user",
                    Label::Task | Label::Continue => "user muted",
                })
                .text(&message.text),
            Unit::Notice(notice) => notice_line(notice),
            Unit::Turn(turn) => turn_card(turn, state),
        })
    };
    match column {
        Column::Windowed => {
            let built = windowed(state.column, units, key, unit, true);
            Effect::new(move |_| state.built.set(built.get()));
        }
        Column::Whole => {
            state.column.each(units, key, unit);
            follow_tail(state.column);
        }
    }
    state.column.child(
        el("div")
            .class("footer")
            .when(
                move || session_end(state.status.get()).is_some(),
                move || match session_end(state.status.get_untracked()) {
                    Some(notice) => notice_line(&notice),
                    None => el("div"),
                },
            )
            .each(
                move || state.tasks.get(),
                |task| chrome::task_line(task),
                |task| el("div").class("task").text(&chrome::task_line(task)),
            ),
    );

    let rows = RwSignal::new(1usize);
    state
        .field
        .attr("rows", "1")
        .attr_dyn("placeholder", move || {
            placeholder(state.status.get()).to_string()
        })
        .attr_dyn("style", move || {
            format!("height: {}px", rows.get() * COMPOSER_LINE)
        })
        .on("input", move |event| {
            if let DomEventData::Input(input) = &event.data {
                state.typed.set(input.value.clone());
            }
            // The field is as tall as its lines, up to a bound; the lines
            // are the editor's own, so a wrapped line counts.
            let field = state.field;
            dispatched(move |page| {
                if let Some(lines) = page.lines(field) {
                    rows.set(lines.clamp(1, COMPOSER_MAX_ROWS));
                }
            });
        })
        .on("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            if key.key == Key::Named(NamedKey::Enter) && !key.modifiers.shift() && !key.is_composing
            {
                prevent_default();
                state.submit();
                rows.set(1);
            }
        });

    let composer = el("div").class("composer").child(
        el("div").class("box").child(state.field).child(
            el("div")
                .class("hints")
                .child(
                    el("span")
                        .class("chip model")
                        .text_dyn(move || state.model.get())
                        .on("click", move |_| state.ask(Asked::SwitchModel)),
                )
                .child(el("span").class("spacer"))
                .child(el("span").class("chip").text("Enter"))
                .child(el("span").text(chrome::SEND_HINT)),
        ),
    );

    let root = el("div")
        .class("agent")
        .on("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            if key.key == Key::Named(NamedKey::Escape) && state.deny_pending() {
                stop_propagation();
            }
        })
        // A press on something that cannot take focus leaves nothing
        // focused, and keys then go to the page instead of the view: the
        // field has the focus again once the press is over.
        .on("pointerup", move |_| {
            let field = state.field;
            dispatched(move |page| {
                if !page.has_focus(field) {
                    field.focus();
                }
            });
        })
        .child(state.column)
        .child(composer);
    (root, state)
}

fn placeholder(status: Status) -> &'static str {
    match status {
        Status::Terminated => chrome::PLACEHOLDER_ENDED,
        Status::Unreachable => chrome::PLACEHOLDER_UNREACHABLE,
        _ => chrome::PLACEHOLDER,
    }
}

/// The notice a session that can no longer run ends its column with.
fn session_end(status: Status) -> Option<Notice> {
    let (tone, text) = match status {
        Status::Terminated => (Tone::Muted, chrome::SESSION_ENDED),
        Status::Unreachable => (Tone::Danger, chrome::RUNTIME_UNREACHABLE),
        _ => return None,
    };
    Some(Notice {
        tone,
        text: text.to_string(),
    })
}

fn notice_line(notice: &Notice) -> El {
    el("div")
        .class(match notice.tone {
            Tone::Muted => "notice",
            Tone::Warning => "notice warning",
            Tone::Danger => "notice danger",
        })
        .text(&notice.text)
}

fn chip(class: &str, label: &str) -> El {
    el("span").class(class).text(label)
}

/// The running session's state, following it.
fn session_state_chip(state: Agent) -> El {
    el("span").class("chip accent").text_dyn(move || {
        if state.status.get() == Status::WaitingForApproval {
            chrome::WAITING_FOR_APPROVAL.to_string()
        } else {
            chrome::RUNNING.to_string()
        }
    })
}

fn stop_button(state: Agent) -> El {
    el("span")
        .class("button")
        .text(chrome::STOP)
        .on("click", move |_| {
            stop_propagation();
            state.ask(Asked::Stop);
        })
}

fn thinking() -> El {
    let dots = el("div").class("thinking").text(chrome::THINKING);
    for dot in ["d d0", "d d1", "d d2"] {
        dots.child(el("span").class(dot).text("・"));
    }
    dots
}

/// One whole turn: its text and its receipt lines in order, its thinking
/// state last. While it runs, the session's state opens its running
/// receipt line and `Stop` ends it (or, with no running receipt, the
/// thinking line); a finished turn ends with its elapsed time at the right.
fn turn_card(turn: &Turn, state: Agent) -> El {
    let running = turn.state == TurnState::Running;
    let carried = move || running && state.status.get().running();
    let carrier = running_receipt(turn).map(|receipt| receipt.key);
    let card = el("div").class("turn");
    if running {
        card.child(el("div").class("bar"));
    }
    for part in &turn.parts {
        match part {
            TurnPart::Text(text) => {
                let body = crate::markdown::render(text, Rc::new(|_| {}));
                card.child(el("div").class("text").child(body));
            }
            TurnPart::Receipt(receipt) => {
                let carries = running && carrier == Some(receipt.key);
                card.child(receipt_block(Rc::new(receipt.clone()), carries, state));
            }
            TurnPart::Thinking => {}
        }
    }
    let thinks = turn.parts.contains(&TurnPart::Thinking);
    if running && carrier.is_none() {
        let line = el("div")
            .class("line")
            .when(carried, move || session_state_chip(state));
        if thinks {
            line.child(thinking());
        }
        line.child(el("span").class("spacer"))
            .when(carried, move || stop_button(state));
        card.child(line);
    }
    if !running {
        let label = match turn.state {
            TurnState::Stopped => Some(("chip", chrome::STOPPED)),
            TurnState::Failed => Some(("chip danger", chrome::FAILED)),
            TurnState::Done | TurnState::Running => None,
        };
        let elapsed = turn
            .elapsed
            .as_ref()
            .filter(|_| turn.state == TurnState::Done);
        if label.is_some() || elapsed.is_some() {
            let end = el("div").class("end");
            if let Some((class, label)) = label {
                end.child(chip(class, label));
            }
            if let Some(elapsed) = elapsed {
                end.child(el("span").text(elapsed));
            }
            card.child(end);
        }
    }
    card
}

/// The last receipt of `turn`, when a call in it is in flight.
fn running_receipt(turn: &Turn) -> Option<&Receipt> {
    (turn.parts.iter().rev())
        .find_map(|part| match part {
            TurnPart::Receipt(receipt) => Some(receipt),
            _ => None,
        })
        .filter(|receipt| receipt.running)
}

/// What a receipt's line reads: while a call in it is in flight, the last
/// such call that is not waiting on an approval; while the only thing left
/// is a call waiting on an approval, that call's name; otherwise its
/// summary.
fn receipt_line_text(receipt: &Receipt) -> String {
    if !receipt.running {
        return receipt.summary.clone();
    }
    let in_flight = (receipt.calls.iter().rev())
        .find(|call| call.status == CallStatus::Running && call.approval.is_none())
        .map(|call| chrome::in_flight(&call.verb, &call.target));
    let awaiting = || {
        (receipt.calls.iter().rev())
            .find(|call| call.approval.is_some())
            .map(|call| format!("{} {}", call.verb, call.target))
    };
    in_flight
        .or_else(awaiting)
        .unwrap_or_else(|| receipt.summary.clone())
}

/// A receipt's line and, once a click has opened it, its calls. The
/// session's state and `Stop` sit on the line when it `carries` them.
fn receipt_block(receipt: Rc<Receipt>, carries: bool, state: Agent) -> El {
    let key = receipt.key;
    let expanded = move || state.expanded.get().contains(&key);
    let carried = move || carries && state.status.get().running();
    let failed = (receipt.calls.iter())
        .filter(|call| call.status == CallStatus::Failed)
        .count();
    let line = el("div")
        .class("receipt-line")
        .on("click", move |_| {
            state.expanded.update(|expanded| {
                if !expanded.remove(&key) {
                    expanded.insert(key);
                }
            });
        })
        .when(carried, move || session_state_chip(state))
        .child(el("span").text(&receipt_line_text(&receipt)));
    if failed > 0 {
        line.child(chip("chip danger", &chrome::failed_calls(failed)));
    }
    line.when(carried, || el("span").class("spacer"))
        .when(carried, move || stop_button(state));

    let block = el("div").class("receipt").child(line);
    // A pending approval sits under the line while that is collapsed, and
    // under its call once it is open.
    let approvals: Vec<(usize, Approval)> = (receipt.calls.iter().enumerate())
        .filter_map(|(index, call)| Some((index, call.approval.clone()?)))
        .collect();
    if !approvals.is_empty() {
        block.each(
            move || {
                if expanded() {
                    Vec::new()
                } else {
                    approvals.clone()
                }
            },
            |(index, _)| *index,
            move |(_, approval)| approval_block(approval, state),
        );
    }
    block.when(expanded, move || {
        let calls = el("div").class("calls");
        for (index, call) in receipt.calls.iter().enumerate() {
            calls.child(call_row(key, index, call, state));
        }
        calls
    })
}

fn call_row(receipt: usize, index: usize, call: &Call, state: Agent) -> El {
    let row = el("div")
        .class(if call.body.is_some() {
            "call-row opens"
        } else {
            "call-row"
        })
        .child(
            el("span")
                .class("dot-cell")
                .child(el("span").class(match call.status {
                    CallStatus::Done | CallStatus::Cancelled => "dot",
                    CallStatus::Failed => "dot failed",
                    CallStatus::Running => "dot running",
                    CallStatus::Denied => "dot denied",
                })),
        )
        .child(el("span").class("verb").text(&call.verb))
        .child(el("span").class("target").text(&call.target));
    let block = el("div").class("call").child(row);
    if let Some(body) = call.body.clone() {
        row.on("click", move |_| {
            state.open_bodies.update(|open| {
                if !open.remove(&(receipt, index)) {
                    open.insert((receipt, index));
                }
            });
        });
        block.when(
            move || state.open_bodies.get().contains(&(receipt, index)),
            move || call_body(&body),
        );
    }
    if let Some(approval) = &call.approval {
        block.child(approval_block(approval, state));
    }
    block
}

/// A pending approval, on one line.
fn approval_block(approval: &Approval, state: Agent) -> El {
    let (approve, deny) = (approval.id.clone(), approval.id.clone());
    el("div")
        .class("approval")
        .child(chip("chip accent", chrome::APPROVAL_NEEDED))
        .child(el("span").class("reason").text(&approval.reason))
        .child(chip("chip", "Enter"))
        .child(
            el("span")
                .class("button primary approve")
                .text(chrome::APPROVE)
                .on("click", move |_| {
                    stop_propagation();
                    state.ask(Asked::Approve(approve.clone()));
                }),
        )
        .child(chip("chip", "Esc"))
        .child(
            el("span")
                .class("button deny")
                .text(chrome::DENY)
                .on("click", move |_| {
                    stop_propagation();
                    state.ask(Asked::Deny(deny.clone()));
                }),
        )
}

fn call_body(body: &CallBody) -> El {
    match body {
        CallBody::Diff(lines) => {
            let diff = el("div").class("diff");
            for line in lines {
                let (class, mark) = match line.kind {
                    DiffKind::Added => ("diff-line added", "+"),
                    DiffKind::Removed => ("diff-line removed", "−"),
                    DiffKind::Context => ("diff-line", ""),
                };
                diff.child(
                    el("div")
                        .class(class)
                        .child(el("span").class("mono mark").text(mark))
                        .child(el("span").class("mono").text(&line.text)),
                );
            }
            diff
        }
        CallBody::Output { lines, omitted } => {
            let output = el("div").class("output");
            for line in lines {
                output.child(el("div").class("mono").text(line));
            }
            let block = el("div").child(output);
            if *omitted > 0 {
                block.child(el("div").class("more").text(&chrome::more_lines(*omitted)));
            }
            block
        }
        CallBody::Text(text) => el("div").class("notice").text(text),
    }
}
