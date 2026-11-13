//! The agent pane: a scrolling column of the conversation's units above
//! a composer.
//!
//! What it draws is the display model both shells share
//! (`horizon-agent-display`) and its fixed labels. The column holds the
//! units near what is in view as elements of the document
//! ([`horizon_ui::windowed`]), or, asked to, all of them. A unit that is the same allocation as before keeps its
//! elements; the turn in progress is a new allocation on every update and
//! is built again.

use std::collections::HashSet;
use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::owner::{LocalStorage, StoredValue};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update, WithValue};
use horizon_ui::windowed::{follow_tail, windowed};
use horizon_ui::{Callback, Component, El, dispatched, el, prevent_default, stop_propagation};

use crate::parts::{self, Button, ButtonKind, ButtonSize, Chip, field, prompt_of};
use keyboard_types::{Key, NamedKey};

use horizon_agent_display::chrome;
use horizon_agent_display::{
    Approval, Call, CallBody, CallStatus, ComposerMode, DiffKind, Label, Notice, Receipt, Status,
    Task, Tone, Transcript, Turn, TurnPart, TurnState, Unit,
};

mod pane;
mod session;

pub use pane::live;
pub use session::{AgentSession, Standing};

const COMPOSER_MAX_ROWS: usize = 8;

pub(crate) mod styles {
    horizon_ui::css_module! {"
    .agent { display: flex; flex-direction: column; height: 100%; min-width: 240px;
             outline: none; background: var(--background); color: var(--text);
             font-family: var(--font-family); font-size: var(--body-size);
             line-height: var(--body-line);
        .column { flex: 1; min-height: 0; overflow-y: scroll;
                  padding: var(--u24) 1px var(--u32) 1px; }
        .unit { padding-bottom: var(--u8); }
        .user { margin-left: var(--u24); padding: var(--u8) var(--u16); border-radius: var(--radius);
                background: var(--band); white-space: pre-wrap; }
        .user.muted { color: var(--text-muted); }
        .turn { position: relative; display: flex; flex-direction: column;
                gap: var(--u4); padding: var(--u8) var(--u16);
                border: var(--line) solid var(--border); border-radius: var(--radius); }
        .bar { position: absolute; left: 0; top: 0; bottom: 0; width: 2px;
               background: var(--accent); }
        .notice, .task { padding: 0 var(--u16); font-size: var(--meta-size);
                         line-height: var(--meta-line); color: var(--text-muted); }
        .footer { display: flex; flex-direction: column; gap: var(--u8); }
        .warning { color: var(--warning); }
        .danger { color: var(--danger); }
        .line { display: flex; align-items: center; gap: var(--u8); }
        .spacer { flex: 1; min-width: 0; }
        .receipt { display: flex; flex-direction: column; gap: var(--u4); }
        .receipt-line { display: flex; align-items: center; gap: var(--u8); cursor: pointer;
                        font-size: var(--meta-size); line-height: var(--meta-line);
                        color: var(--text-muted); }
        .calls { display: flex; flex-direction: column; gap: var(--u4);
                 margin-left: var(--u8); padding-left: var(--u16);
                 border-left: var(--line) solid var(--border); }
        .call { display: flex; flex-direction: column; gap: var(--u4); }
        .call-row { display: flex; align-items: center; gap: var(--u8);
                    font-size: var(--meta-size); line-height: var(--meta-line); }
        .call-row.opens { cursor: pointer; }
        .dot-cell { flex: none; width: var(--u16); display: flex; align-items: center;
                    justify-content: center; }
        .dot { width: var(--u8); height: var(--u8); border-radius: var(--u4);
               background: var(--text-muted); }
        .dot.failed { background: var(--danger); }
        .dot.running { background: var(--accent); }
        .dot.denied { background: var(--warning); }
        .verb { flex: none; color: var(--text-muted); }
        .target { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;
                  white-space: nowrap; }
        .mono { font-size: var(--meta-size); line-height: var(--meta-line);
                min-height: var(--meta-line); white-space: pre; }
        .diff-line { display: flex; color: var(--text-muted); }
        .diff-line.added { background: var(--diff-added-surface);
                           color: var(--diff-added-text); }
        .diff-line.removed { background: var(--diff-removed-surface);
                             color: var(--diff-removed-text); }
        .mark { flex: none; width: var(--u16); }
        .output { overflow-x: scroll; color: var(--text-muted); }
        .more { font-size: var(--micro-size); line-height: var(--micro-line);
                color: var(--text-muted); }
        .approval { display: flex; align-items: center; gap: var(--u8);
                    font-size: var(--meta-size); line-height: var(--meta-line); }
        .reason { flex: 1; min-width: 0; }
        .end { display: flex; justify-content: flex-end; align-items: baseline;
               gap: var(--u8); font-size: var(--meta-size); line-height: var(--meta-line);
               color: var(--text-muted); }
        .thinking { display: flex; font-size: var(--micro-size);
                    line-height: var(--micro-line); color: var(--text-muted); }
        .thinking .d { animation: thinking 1.2s linear infinite; }
        .thinking .d1 { animation-delay: -0.8s; }
        .thinking .d2 { animation-delay: -0.4s; }
        .composer { flex: none; padding: 0 var(--u24) var(--u12) var(--u24); }
        .box { display: flex; flex-direction: column; gap: var(--u4);
               padding: var(--u8) var(--u16); border: var(--line) solid var(--border);
               border-radius: var(--radius); }
        .field { padding: 6px var(--u12); }
        .hints { display: flex; align-items: center; gap: var(--u4);
                 font-size: var(--micro-size); line-height: var(--micro-line);
                 color: var(--text-muted); }
        .model { cursor: pointer; }
    }
    @keyframes thinking { from { opacity: 1; } to { opacity: 0.25; } }
"}
}

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
    pub fn set_transcript(&self, transcript: Transcript) {
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
        column: el("div").class(styles::COLUMN),
        field: el("textarea"),
    };

    let units = move || (state.units.get().into_iter().enumerate()).collect::<Vec<_>>();
    // A unit kept from the last update is the allocation it was.
    let key = |(index, unit): &(usize, Rc<Unit>)| (*index, Rc::as_ptr(unit) as usize);
    let unit = move |(_, unit): &(usize, Rc<Unit>)| {
        el("div").class(styles::UNIT).child(match unit.as_ref() {
            Unit::User(message) => el("div")
                .class((
                    styles::USER,
                    match message.label {
                        Label::You => None,
                        Label::Task | Label::Continue => Some(styles::MUTED),
                    },
                ))
                .text(&message.text),
            Unit::Notice(notice) => notice_line(notice),
            Unit::Turn(turn) => turn_card(turn, state),
        })
    };
    match column {
        Column::Windowed => {
            windowed(state.column, units, key, unit, true, state.built);
        }
        Column::Whole => {
            state.column.each(units, key, unit);
            follow_tail(state.column);
        }
    }
    state.column.child(
        el("div")
            .class(styles::FOOTER)
            .when(
                move || session_end(state.status.get()).is_some(),
                move || match session_end(state.status.get_untracked()) {
                    Some(notice) => notice_line(&notice),
                    None => el("div"),
                },
            )
            .each(
                move || state.tasks.get(),
                chrome::task_line,
                |task| el("div").class(styles::TASK).text(&chrome::task_line(task)),
            ),
    );

    let rows = RwSignal::new(1usize);
    state
        .field
        .attr("rows", "1")
        .attr_dyn("style", move || {
            format!("height: calc(var(--body-line) * {})", rows.get())
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

    let composer = el("div").class(styles::COMPOSER).child(
        el("div")
            .class(styles::BOX)
            .child(
                el("div")
                    .class(styles::FIELD)
                    .child(field(state.field).when(
                        move || state.typed.get().is_empty(),
                        move || {
                            prompt_of()
                                .text_dyn(move || placeholder(state.status.get()).to_string())
                        },
                    )),
            )
            .child(
                el("div")
                    .class(styles::HINTS)
                    .child(
                        el("span")
                            .class(("chip", styles::MODEL))
                            .text_dyn(move || state.model.get())
                            .on("click", move |_| state.ask(Asked::SwitchModel)),
                    )
                    .child(el("span").class(styles::SPACER))
                    .child(el("span").class("chip").text("Enter"))
                    .child(el("span").text(chrome::SEND_HINT)),
            ),
    );

    let root = el("div")
        .class(styles::AGENT)
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
        .class((
            styles::NOTICE,
            match notice.tone {
                Tone::Muted => None,
                Tone::Warning => Some(styles::WARNING),
                Tone::Danger => Some(styles::DANGER),
            },
        ))
        .text(&notice.text)
}

/// The running session's state, following it.
fn session_state_chip(state: Agent) -> El {
    Chip {
        tone: parts::Tone::Accent,
        text: "",
    }
    .view()
    .text_dyn(move || {
        if state.status.get() == Status::WaitingForApproval {
            chrome::WAITING_FOR_APPROVAL.to_string()
        } else {
            chrome::RUNNING.to_string()
        }
    })
}

fn stop_button(state: Agent) -> El {
    Button {
        label: chrome::STOP,
        size: ButtonSize::Small,
        on_press: Some(Callback::new(move |()| {
            stop_propagation();
            state.ask(Asked::Stop);
        })),
        ..Default::default()
    }
    .view()
}

fn thinking() -> El {
    let dots = el("div").class(styles::THINKING).text(chrome::THINKING);
    // (The first has no delay of its own.)
    for dot in [None, Some(styles::D1), Some(styles::D2)] {
        dots.child(el("span").class((styles::D, dot)).text("・"));
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
    let card = el("div").class(styles::TURN);
    if running {
        card.child(el("div").class(styles::BAR));
    }
    for part in &turn.parts {
        match part {
            TurnPart::Text(text) => {
                let body = crate::markdown::render(
                    text,
                    crate::markdown::Headings::Scaled,
                    Rc::new(|_| {}),
                );
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
            .class(styles::LINE)
            .when(carried, move || session_state_chip(state));
        if thinks {
            line.child(thinking());
        }
        line.child(el("span").class(styles::SPACER))
            .when(carried, move || stop_button(state));
        card.child(line);
    } else if thinks {
        // The state and `Stop` are on the running receipt's line: the
        // thinking is a line of its own.
        card.child(thinking());
    }
    if !running {
        let label = match turn.state {
            TurnState::Stopped => Some((parts::Tone::Muted, chrome::STOPPED)),
            TurnState::Failed => Some((parts::Tone::Danger, chrome::FAILED)),
            TurnState::Done | TurnState::Running => None,
        };
        let elapsed = turn
            .elapsed
            .as_ref()
            .filter(|_| turn.state == TurnState::Done);
        if label.is_some() || elapsed.is_some() {
            let end = el("div").class(styles::END);
            if let Some((tone, label)) = label {
                end.child(Chip { tone, text: label });
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
        .class(styles::RECEIPT_LINE)
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
        line.child(Chip {
            tone: parts::Tone::Danger,
            text: &chrome::failed_calls(failed),
        });
    }
    line.when(carried, || el("span").class(styles::SPACER))
        .when(carried, move || stop_button(state));

    let block = el("div").class(styles::RECEIPT).child(line);
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
        let calls = el("div").class(styles::CALLS);
        for (index, call) in receipt.calls.iter().enumerate() {
            calls.child(call_row(key, index, call, state));
        }
        calls
    })
}

fn call_row(receipt: usize, index: usize, call: &Call, state: Agent) -> El {
    let row = el("div")
        .class((
            styles::CALL_ROW,
            call.body.is_some().then_some(styles::OPENS),
        ))
        .child(el("span").class(styles::DOT_CELL).child(el("span").class((
            styles::DOT,
            match call.status {
                CallStatus::Done | CallStatus::Cancelled => None,
                CallStatus::Failed => Some(styles::FAILED),
                CallStatus::Running => Some(styles::RUNNING),
                CallStatus::Denied => Some(styles::DENIED),
            },
        ))))
        .child(el("span").class(styles::VERB).text(&call.verb))
        .child(el("span").class(styles::TARGET).text(&call.target));
    let block = el("div").class(styles::CALL).child(row);
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
        .class(styles::APPROVAL)
        .child(Chip {
            tone: parts::Tone::Accent,
            text: chrome::APPROVAL_NEEDED,
        })
        .child(el("span").class(styles::REASON).text(&approval.reason))
        .child(Chip {
            tone: parts::Tone::Muted,
            text: "Enter",
        })
        .child(
            Button {
                label: chrome::APPROVE,
                kind: ButtonKind::Primary,
                on_press: Some(Callback::new(move |()| {
                    stop_propagation();
                    state.ask(Asked::Approve(approve.clone()));
                })),
                ..Default::default()
            }
            .class("approve"),
        )
        .child(Chip {
            tone: parts::Tone::Muted,
            text: "Esc",
        })
        .child(
            Button {
                label: chrome::DENY,
                on_press: Some(Callback::new(move |()| {
                    stop_propagation();
                    state.ask(Asked::Deny(deny.clone()));
                })),
                ..Default::default()
            }
            .class("deny"),
        )
}

fn call_body(body: &CallBody) -> El {
    match body {
        CallBody::Diff(lines) => {
            let diff = el("div").class("diff");
            for line in lines {
                let (kind, mark) = match line.kind {
                    DiffKind::Added => (Some(styles::ADDED), "+"),
                    DiffKind::Removed => (Some(styles::REMOVED), "−"),
                    DiffKind::Context => (None, ""),
                };
                diff.child(
                    el("div")
                        .class((styles::DIFF_LINE, kind))
                        .child(el("span").class((styles::MONO, styles::MARK)).text(mark))
                        .child(el("span").class(styles::MONO).text(&line.text)),
                );
            }
            diff
        }
        CallBody::Output { lines, omitted } => {
            let output = el("div").class(styles::OUTPUT);
            for line in lines {
                output.child(el("div").class(styles::MONO).text(line));
            }
            let block = el("div").child(output);
            if *omitted > 0 {
                block.child(
                    el("div")
                        .class(styles::MORE)
                        .text(&chrome::more_lines(*omitted)),
                );
            }
            block
        }
        CallBody::Text(text) => el("div").class(styles::NOTICE).text(text),
    }
}
