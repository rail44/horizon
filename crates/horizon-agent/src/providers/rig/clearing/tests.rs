use rig_core::completion::message::{ToolCall, ToolFunction};

use super::*;
use crate::config::{resolve_clearing_threshold_pct, RigAgentConfig, DEFAULT_CLEARING_TRIGGER_PCT};
use crate::contract::{SessionId, ToolCallRequest};
use crate::tools::MemoryDocument;

/// One token of tail budget is worth this many characters, per
/// [`CLEARING_CHARS_PER_TOKEN`]. Spelled out here so the fixtures below read
/// as "n tokens' worth of text".
fn chars_for_tokens(tokens: u64) -> usize {
    (tokens * CLEARING_CHARS_PER_TOKEN) as usize
}

fn tool_call_message(calls: &[(&str, &str, serde_json::Value)]) -> Message {
    Message::Assistant {
        id: None,
        content: OneOrMany::many(
            calls
                .iter()
                .map(|(call_id, tool_id, args)| {
                    AssistantContent::ToolCall(ToolCall::new(
                        (*call_id).to_string(),
                        ToolFunction::new((*tool_id).to_string(), args.clone()),
                    ))
                })
                .collect::<Vec<_>>(),
        )
        .expect("at least one call"),
    }
}

fn tool_result_message(call_id: &str, chars: usize) -> Message {
    Message::tool_result(call_id, "x".repeat(chars))
}

fn call_id(id: &str) -> ToolCallId {
    ToolCallId(id.to_string())
}

fn cleared_set(ids: &[&str]) -> ClearedResults {
    ClearedResults::from_occurrences(ids.iter().map(|id| call_id(id)))
}

/// The text of the (single) tool result carried by `message`, if it is one.
fn tool_result_text(message: &Message) -> Option<String> {
    let Message::User { content } = message else {
        return None;
    };
    content.iter().find_map(|item| match item {
        UserContent::ToolResult(result) => Some(
            result
                .content
                .iter()
                .filter_map(|item| match item {
                    ToolResultContent::Text(text) => Some(text.text.clone()),
                    ToolResultContent::Image(_) => None,
                })
                .collect::<String>(),
        ),
        _ => None,
    })
}

/// A history with `count` `fs.read` rounds, each one assistant tool call
/// followed by a result of `chars` characters, plus an opening user
/// message. The final round is deliberately *not* left unresolved -- tests
/// that care about the current-round protection build that shape
/// explicitly.
fn read_rounds(count: usize, chars: usize) -> Vec<Message> {
    let mut history = vec![Message::user("audit these files")];
    for index in 0..count {
        let id = format!("call-{index}");
        history.push(tool_call_message(&[(
            &id,
            "fs.read",
            serde_json::json!({ "path": format!("src/file-{index}.rs") }),
        )]));
        history.push(tool_result_message(&id, chars));
    }
    history
}

// --- trigger math ----------------------------------------------------------

#[test]
fn an_unknown_window_never_fires_however_large_the_input_gets() {
    // crush's `cw == 0` protection: no declared limits means no clearing at
    // all, not a guessed window.
    let mut state = ClearingState::new(None, DEFAULT_CLEARING_TRIGGER_PCT);
    state.record_input_tokens(10_000_000);
    let history = read_rounds(40, chars_for_tokens(4_000));

    assert!(state.run_pass(&history).is_none());
    assert!(state.cleared().is_empty());
}

#[test]
fn input_below_the_threshold_share_of_the_window_does_not_fire() {
    let window = 200_000;
    let mut state = ClearingState::new(Some(window), 60);
    // 59.9% of the window: under the trigger, however much is recoverable.
    state.record_input_tokens(window * 599 / 1000);
    let history = read_rounds(40, chars_for_tokens(4_000));

    assert!(state.run_pass(&history).is_none());
}

#[test]
fn input_at_the_threshold_share_fires() {
    let window = 200_000;
    let mut state = ClearingState::new(Some(window), 60);
    state.record_input_tokens(window * 60 / 100);
    let history = read_rounds(40, chars_for_tokens(4_000));

    let cleared = state
        .run_pass(&history)
        .expect("exactly at the threshold must count as reaching it");
    assert!(!cleared.cleared_call_ids.is_empty());
}

#[test]
fn a_pass_below_the_recovery_floor_does_not_fire_even_over_the_threshold() {
    // Over the trigger, but the only clearable text is far under the 16k
    // recovery floor -- clearing here would spend the whole prompt cache to
    // buy almost nothing (OpenCode's PRUNE_MINIMUM reasoning).
    let window = 200_000;
    let mut state = ClearingState::new(Some(window), 60);
    state.record_input_tokens(window);
    let history = read_rounds(30, chars_for_tokens(600));

    assert!(state.run_pass(&history).is_none());
    assert!(state.cleared().is_empty());
}

#[test]
fn the_env_override_moves_the_trigger_and_rejects_nonsense() {
    assert_eq!(resolve_clearing_threshold_pct(None), 60);
    assert_eq!(resolve_clearing_threshold_pct(Some("5".to_string())), 5);
    assert_eq!(
        resolve_clearing_threshold_pct(Some(" 100 ".to_string())),
        100
    );
    // Out of range, zero, and non-numeric all leave the default in place
    // rather than producing a threshold nothing designed.
    assert_eq!(resolve_clearing_threshold_pct(Some("0".to_string())), 60);
    assert_eq!(resolve_clearing_threshold_pct(Some("101".to_string())), 60);
    assert_eq!(resolve_clearing_threshold_pct(Some("-1".to_string())), 60);
    assert_eq!(
        resolve_clearing_threshold_pct(Some("sixty".to_string())),
        60
    );

    // And the resolved percentage really is what the trigger uses: 5% of
    // the window fires where the built-in 60% would not have.
    let window = 200_000;
    let mut state = ClearingState::new(Some(window), 5);
    state.record_input_tokens(window / 10);
    let history = read_rounds(40, chars_for_tokens(4_000));
    assert!(state.run_pass(&history).is_some());
}

// --- protection invariants -------------------------------------------------

#[test]
fn the_tail_budget_keeps_the_most_recent_results_verbatim() {
    // 40 rounds of 4k tokens each. The tail keeps whole rounds until their
    // combined size reaches the 16k-token budget -- the round that crosses
    // it is kept too, so the boundary never lands mid-round.
    let per_round_tokens = 4_000;
    let history = read_rounds(40, chars_for_tokens(per_round_tokens));
    let plan = plan_clearing_pass(&history, &ClearedResults::default());

    let expected_tail = CLEARING_TAIL_BUDGET_TOKENS.div_ceil(per_round_tokens) as usize;
    assert_eq!(plan.cleared_call_ids.len(), 40 - expected_tail);
    assert_eq!(plan.cleared_call_ids.first(), Some(&call_id("call-0")));
    assert_eq!(
        plan.cleared_call_ids.last(),
        Some(&call_id(&format!("call-{}", 40 - expected_tail - 1))),
        "the pass walks oldest-first and stops at the tail boundary"
    );
    assert_eq!(
        plan.recovered_chars,
        ((40 - expected_tail) * chars_for_tokens(per_round_tokens)) as u64
    );
}

#[test]
fn a_history_smaller_than_the_tail_budget_clears_nothing() {
    let history = read_rounds(3, chars_for_tokens(1_000));
    assert_eq!(
        plan_clearing_pass(&history, &ClearedResults::default()),
        ClearingPlan::default()
    );
}

#[test]
fn the_current_unresolved_round_is_never_cleared() {
    // A parallel batch whose results are still landing: the last assistant
    // tool-call message opened calls A/B/C, two of their results are
    // already folded into history, and the third is about to arrive as the
    // next request's prompt. None of the three may be touched, even though
    // a tiny tail budget would otherwise leave them eligible.
    let mut history = read_rounds(40, chars_for_tokens(4_000));
    history.push(tool_call_message(&[
        ("batch-a", "fs.read", serde_json::json!({ "path": "a.rs" })),
        ("batch-b", "fs.read", serde_json::json!({ "path": "b.rs" })),
        ("batch-c", "fs.read", serde_json::json!({ "path": "c.rs" })),
    ]));
    history.push(tool_result_message("batch-a", chars_for_tokens(40_000)));
    history.push(tool_result_message("batch-b", chars_for_tokens(40_000)));

    let plan = plan_clearing_pass(&history, &ClearedResults::default());
    for id in ["batch-a", "batch-b", "batch-c"] {
        assert!(
            !plan.cleared_call_ids.contains(&call_id(id)),
            "{id} belongs to the round still resolving"
        );
    }
    // The older rounds are still eligible -- the batch's own huge results
    // consumed the tail budget, so everything before them qualifies.
    assert_eq!(plan.cleared_call_ids.len(), 40);
}

#[test]
fn non_tool_messages_are_structurally_out_of_scope() {
    // The brief, a mid-run user interjection, a `task` notification (which
    // replays as a user-role text message), and assistant prose all survive
    // a pass over a history large enough to trigger one -- they are not
    // tool results, so nothing selects them.
    let mut history = vec![
        Message::user("THE BRIEF: implement tier 1"),
        Message::assistant("understood"),
    ];
    history.extend(read_rounds(40, chars_for_tokens(4_000)));
    history.push(Message::user("task `abcd` finished: report follows"));
    history.push(Message::user("also check the docs"));

    let cleared = ClearedResults::from_occurrences(
        plan_clearing_pass(&history, &ClearedResults::default()).cleared_call_ids,
    );
    let projected = history_for_provider_request(&history, &cleared, None);

    assert_eq!(projected[0], history[0]);
    assert_eq!(projected[1], history[1]);
    assert_eq!(projected[projected.len() - 2], history[history.len() - 2]);
    assert_eq!(projected[projected.len() - 1], history[history.len() - 1]);
}

#[test]
fn clearing_preserves_every_tool_call_result_pair() {
    let history = read_rounds(40, chars_for_tokens(4_000));
    let cleared = ClearedResults::from_occurrences(
        plan_clearing_pass(&history, &ClearedResults::default()).cleared_call_ids,
    );
    assert!(!cleared.is_empty());
    let projected = history_for_provider_request(&history, &cleared, None);

    assert_eq!(projected.len(), history.len(), "no message is ever dropped");
    for (index, (before, after)) in history.iter().zip(&projected).enumerate() {
        match (before, after) {
            // Assistant tool-call messages are untouched, call ids included:
            // splitting a call from its result is a known provider-400
            // source, so the projection never reshapes this side.
            (Message::Assistant { .. }, _) => assert_eq!(before, after, "message {index}"),
            (Message::User { content: before }, Message::User { content: after }) => {
                let ids = |content: &OneOrMany<UserContent>| {
                    content
                        .iter()
                        .filter_map(|item| match item {
                            UserContent::ToolResult(result) => Some(result.id.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(
                    ids(before),
                    ids(after),
                    "message {index} keeps its call ids"
                );
            }
            _ => panic!("message {index} changed shape"),
        }
    }
}

// --- the frozen set --------------------------------------------------------

#[test]
fn two_consecutive_request_builds_are_byte_identical() {
    // The stable-prefix property the whole freeze exists for: with the set
    // unchanged, the projection is a pure function of history, so the
    // provider's cache is invalidated once per pass and not per round.
    let history = read_rounds(40, chars_for_tokens(4_000));
    let cleared = ClearedResults::from_occurrences(
        plan_clearing_pass(&history, &ClearedResults::default()).cleared_call_ids,
    );

    let first = history_for_provider_request(&history, &cleared, None);
    let second = history_for_provider_request(&history, &cleared, None);
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
}

#[test]
fn a_second_pass_extends_the_set_and_never_re_reports_what_it_already_cleared() {
    let window = 200_000;
    let mut state = ClearingState::new(Some(window), 60);
    state.record_input_tokens(window);

    let history = read_rounds(40, chars_for_tokens(4_000));
    let first = state.run_pass(&history).expect("the first pass fires");
    let after_first: ClearedResults = state.cleared().clone();
    assert_eq!(
        after_first.len(),
        first.cleared_call_ids.len(),
        "the frozen set is exactly what the pass reported"
    );

    // The turn loop moved on: more rounds landed, so more of the history is
    // now behind the tail budget.
    let mut grown = history.clone();
    for index in 40..80 {
        let id = format!("late-{index}");
        grown.push(tool_call_message(&[(
            &id,
            "fs.grep",
            serde_json::json!({ "pattern": "TODO" }),
        )]));
        grown.push(tool_result_message(&id, chars_for_tokens(4_000)));
    }
    let second = state.run_pass(&grown).expect("the second pass fires");

    for id in &first.cleared_call_ids {
        assert!(
            !second.cleared_call_ids.contains(id),
            "an already-cleared result must not be reported again"
        );
        assert!(state.cleared().contains(id), "the set only ever grows");
    }
    assert!(state.cleared().len() > after_first.len());
}

#[test]
fn resume_replays_the_frozen_set_into_an_identical_projection() {
    let window = 200_000;
    let mut live = ClearingState::new(Some(window), 60);
    live.record_input_tokens(window);
    let history = read_rounds(40, chars_for_tokens(4_000));
    let event = Event::HistoryCleared(live.run_pass(&history).expect("the pass fires"));

    // A fresh session process reloads canonical history in full and
    // replays the pass event -- and must project exactly what the still-
    // running session would send.
    let mut resumed = ClearingState::new(Some(window), 60);
    resumed.seed_cleared(cleared_call_ids_from_events(std::slice::from_ref(&event)));

    assert_eq!(resumed.cleared(), live.cleared());
    assert_eq!(
        serde_json::to_string(&history_for_provider_request(
            &history,
            resumed.cleared(),
            None
        ))
        .unwrap(),
        serde_json::to_string(&history_for_provider_request(
            &history,
            live.cleared(),
            None
        ))
        .unwrap()
    );
}

#[test]
fn replaying_several_passes_accumulates_every_frozen_set() {
    let events = vec![
        Event::HistoryCleared(HistoryCleared {
            cleared_call_ids: vec![call_id("a"), call_id("b")],
            recovered_chars: 10,
        }),
        Event::TurnEnded(crate::contract::TurnEndReason::Completed),
        Event::HistoryCleared(HistoryCleared {
            cleared_call_ids: vec![call_id("c")],
            recovered_chars: 5,
        }),
    ];
    assert_eq!(
        cleared_call_ids_from_events(&events),
        vec![call_id("a"), call_id("b"), call_id("c")]
    );
}

// --- placeholder shape -----------------------------------------------------

#[test]
fn the_placeholder_names_the_tool_the_key_argument_and_the_recovery_route() {
    let history = vec![
        Message::user("read it"),
        tool_call_message(&[(
            "call-0",
            "fs.read",
            serde_json::json!({ "path": "crates/horizon-agent/src/config.rs", "limit": 500 }),
        )]),
        tool_result_message("call-0", 12_345),
    ];
    let projected = history_for_provider_request(&history, &cleared_set(&["call-0"]), None);
    let text = tool_result_text(&projected[2]).expect("still a tool result");

    assert_eq!(
        text,
        "[cleared old tool result: fs.read path=\"crates/horizon-agent/src/config.rs\" \
         (12345 chars). The full result is retained in the session event log — use \
         recall.search / recall.read, or re-run the tool.]"
    );
    assert_eq!(
        text.lines().count(),
        1,
        "the placeholder is one line, not a block"
    );
}

#[test]
fn the_placeholder_falls_back_to_the_tool_id_and_then_to_nothing() {
    let history = vec![
        tool_call_message(&[("call-0", "workspace.snapshot", serde_json::json!({}))]),
        tool_result_message("call-0", 99),
        tool_result_message("orphan", 99),
    ];
    let projected =
        history_for_provider_request(&history, &cleared_set(&["call-0", "orphan"]), None);

    assert!(tool_result_text(&projected[1])
        .unwrap()
        .starts_with("[cleared old tool result: workspace.snapshot (99 chars)."));
    // No matching call in history (a shape only skew or a truncated log
    // could produce): still a well-formed placeholder, just unnamed.
    assert!(tool_result_text(&projected[2])
        .unwrap()
        .starts_with("[cleared old tool result: tool call (99 chars)."));
}

#[test]
fn a_long_key_argument_is_truncated_in_the_placeholder() {
    let long = "z".repeat(400);
    let history = vec![
        tool_call_message(&[(
            "call-0",
            "bash",
            serde_json::json!({ "command": long.clone() }),
        )]),
        tool_result_message("call-0", 5_000),
    ];
    let text = tool_result_text(
        &history_for_provider_request(&history, &cleared_set(&["call-0"]), None)[1],
    )
    .unwrap();

    assert!(text.contains(&format!("command=\"{}…\"", "z".repeat(120))));
    assert!(text.chars().count() < 300);
}

#[test]
fn an_uncleared_result_is_left_exactly_as_it_was() {
    let history = read_rounds(3, 100);
    assert_eq!(
        history_for_provider_request(&history, &ClearedResults::default(), None),
        history
    );
    assert_eq!(
        history_for_provider_request(&history, &cleared_set(&["not-in-this-history"]), None),
        history
    );
}

// --- call_id reuse ---------------------------------------------------------

/// The hazard the occurrence counting exists for: providers reuse `call_id`s
/// across turns (measured on Kimi, whose ids are `functions.<name>:<index>`),
/// so a fresh result arriving under an id a pass already froze must survive
/// the projection untouched -- blanking it would hand the model a
/// placeholder for a different call in the very turn it needs the output.
#[test]
fn a_result_reusing_a_cleared_call_id_after_the_freeze_is_never_replaced() {
    let mut history = vec![
        Message::user("read it"),
        tool_call_message(&[(
            "functions.fs.read:1",
            "fs.read",
            serde_json::json!({ "path": "src/old.rs" }),
        )]),
        tool_result_message("functions.fs.read:1", 5_000),
    ];
    let cleared = cleared_set(&["functions.fs.read:1"]);

    // A later turn reuses the same provider-supplied id for a genuinely
    // different call.
    history.push(tool_call_message(&[(
        "functions.fs.read:1",
        "fs.read",
        serde_json::json!({ "path": "src/fresh.rs" }),
    )]));
    history.push(Message::tool_result(
        "functions.fs.read:1",
        "the fresh body the model just asked for",
    ));

    let projected = history_for_provider_request(&history, &cleared, None);
    assert!(
        tool_result_text(&projected[2])
            .unwrap()
            .starts_with("[cleared old tool result:"),
        "the occurrence the pass actually froze is still cleared"
    );
    assert_eq!(
        tool_result_text(&projected[4]).unwrap(),
        "the fresh body the model just asked for",
        "a post-freeze result reusing a cleared call_id must be left verbatim"
    );
}

/// The counting is per occurrence, not per id: a pass that clears two
/// same-id results clears exactly those two, and a third still survives.
#[test]
fn a_pass_that_clears_two_occurrences_of_one_id_leaves_a_later_third_verbatim() {
    let mut history = vec![Message::user("read it")];
    for _ in 0..2 {
        history.push(tool_call_message(&[(
            "dup",
            "fs.read",
            serde_json::json!({ "path": "src/old.rs" }),
        )]));
        history.push(tool_result_message("dup", 5_000));
    }
    let cleared = ClearedResults::from_occurrences([call_id("dup"), call_id("dup")]);

    history.push(tool_call_message(&[(
        "dup",
        "fs.read",
        serde_json::json!({ "path": "src/fresh.rs" }),
    )]));
    history.push(Message::tool_result("dup", "fresh"));

    let projected = history_for_provider_request(&history, &cleared, None);
    for index in [2, 4] {
        assert!(tool_result_text(&projected[index])
            .unwrap()
            .starts_with("[cleared old tool result:"));
    }
    assert_eq!(tool_result_text(&projected[6]).unwrap(), "fresh");
}

/// Resume must reproduce the guard, not just the set of ids: the per-id
/// occurrence counts ride the existing `HistoryCleared::cleared_call_ids`
/// list (one entry per cleared occurrence), so replaying the events
/// rebuilds the identical projection -- including leaving the reused id's
/// fresh result alone.
#[test]
fn resume_replay_preserves_the_reuse_guard() {
    let mut history = vec![Message::user("read it")];
    for _ in 0..2 {
        history.push(tool_call_message(&[(
            "dup",
            "fs.read",
            serde_json::json!({ "path": "src/old.rs" }),
        )]));
        history.push(tool_result_message("dup", 5_000));
    }
    history.push(tool_call_message(&[(
        "dup",
        "fs.read",
        serde_json::json!({ "path": "src/fresh.rs" }),
    )]));
    history.push(Message::tool_result("dup", "fresh"));

    let events = vec![Event::HistoryCleared(HistoryCleared {
        cleared_call_ids: vec![call_id("dup"), call_id("dup")],
        recovered_chars: 10_000,
    })];
    let mut resumed = ClearingState::new(Some(200_000), 60);
    resumed.seed_cleared(cleared_call_ids_from_events(&events));

    assert_eq!(
        resumed.cleared(),
        &ClearedResults::from_occurrences([call_id("dup"), call_id("dup")])
    );
    let projected = history_for_provider_request(&history, resumed.cleared(), None);
    assert_eq!(
        tool_result_text(&projected[6]).unwrap(),
        "fresh",
        "a resumed session must apply the same occurrence guard a live one does"
    );
}

// --- the `task_output` synergy ---------------------------------------------

#[test]
fn clearing_an_old_task_report_leaves_task_output_able_to_re_fetch_it() {
    // `docs/agent-compaction-design.md` leans on the originals surviving:
    // a cleared `task` report is still reachable, because a finished child
    // is retained for the requester's lifetime and clearing only rewrites
    // one request's message list.
    let requester = SessionId::new();
    let child = SessionId::new();
    let report = "the child's full findings".repeat(200);
    crate::tools::explore::register_finished_child_for_test(
        requester,
        child,
        "audit the parser",
        serde_json::json!({ "report": report.clone(), "is_error": false }),
    );

    let history = vec![
        Message::user("delegate the audit"),
        tool_call_message(&[(
            "task-1",
            "task",
            serde_json::json!({ "description": "audit the parser", "prompt": "..." }),
        )]),
        tool_result_message("task-1", report.chars().count()),
    ];
    let projected = history_for_provider_request(&history, &cleared_set(&["task-1"]), None);
    assert!(tool_result_text(&projected[2])
        .unwrap()
        .starts_with("[cleared old tool result: task ("));

    let fetch = crate::tools::explore::output(
        requester,
        &ToolCallRequest {
            call_id: call_id("fetch-1"),
            tool_id: crate::tools::TASK_OUTPUT_TOOL_ID.to_string(),
            input: serde_json::json!({ "session_id": child.as_uuid().to_string() }).into(),
            occurrence_id: None,
        },
    );
    let crate::tools::Execution::Auto(events) = fetch else {
        panic!("task_output resolves synchronously");
    };
    let output = events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallFinished(result) => Some(result.output.0.clone()),
            _ => None,
        })
        .expect("task_output finishes with a result");
    assert_eq!(output["status"], "finished");
    assert_eq!(output["report"], serde_json::Value::String(report));
}

// --- the turn loop ---------------------------------------------------------

/// End to end through the real turn entry point (`complete_rig_turn`), in
/// deterministic fallback mode so no provider is involved: a session that
/// has crossed the threshold runs **exactly one** pass, emits exactly one
/// `HistoryCleared`, and keeps turning normally afterwards.
#[tokio::test]
async fn a_session_over_the_threshold_runs_one_pass_and_keeps_turning() {
    let config = RigAgentConfig {
        openai_enabled: false,
        ..Default::default()
    };
    let environment = crate::prompt::SessionEnvironment::for_workspace_root(None);
    let extra_sections: Vec<String> = Vec::new();
    let (events_tx, events_rx) = crossbeam_channel::unbounded();
    let token = tokio_util::sync::CancellationToken::new();

    let window = 200_000;
    let mut clearing = ClearingState::new(Some(window), 60);
    clearing.record_input_tokens(window * 70 / 100);
    let mut history = read_rounds(40, chars_for_tokens(4_000));
    let before = history.len();

    for round in 0..3 {
        super::super::complete_rig_turn(
            &config,
            &environment,
            &extra_sections,
            &mut history,
            Message::user(format!("round {round}")),
            &events_tx,
            &mut clearing,
            None,
            || Message::assistant("ok"),
            &token,
        )
        .await;
    }

    let passes: Vec<HistoryCleared> = events_rx
        .try_iter()
        .filter_map(|event| match event.event {
            Event::HistoryCleared(cleared) => Some(cleared),
            _ => None,
        })
        .collect();
    assert_eq!(
        passes.len(),
        1,
        "the pass is once-per-crossing, not once-per-round"
    );
    assert!(!passes[0].cleared_call_ids.is_empty());
    assert!(passes[0].recovered_chars > 0);
    // The turns themselves went on normally: each appended its prompt and
    // the assistant reply to canonical history, and canonical history is
    // still full-size (clearing is a projection).
    assert_eq!(history.len(), before + 6);
    assert_eq!(
        tool_result_text(&history[2]).map(|text| text.chars().count()),
        Some(chars_for_tokens(4_000)),
        "canonical history keeps the original result body"
    );
}

// --- Standing-agent memory projection ---------------------------------------

/// A non-empty memory document.
fn memory_doc() -> MemoryDocument {
    MemoryDocument {
        goal: "Ship the memory feature".to_string(),
        ..Default::default()
    }
}

/// Extracts the text of a user text message (not a tool result).
fn user_text(message: &Message) -> Option<String> {
    let Message::User { content } = message else {
        return None;
    };
    content.iter().find_map(|item| match item {
        UserContent::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

/// Without a memory document, the projection returns the full history
/// unchanged (when nothing is cleared).
#[test]
fn memory_projection_none_returns_full_history() {
    let history = vec![
        Message::user("turn 1"),
        Message::assistant("reply 1"),
        Message::user("turn 2"),
        Message::assistant("reply 2"),
    ];
    let cleared = ClearedResults::default();
    let projected = history_for_provider_request(&history, &cleared, None);
    assert_eq!(projected.len(), history.len());
}

/// An empty memory document is skipped — the projection returns the full
/// history, so the first turn (before any update) is not stripped.
#[test]
fn memory_projection_empty_document_skips_prepend() {
    let history = vec![
        Message::user("turn 1"),
        Message::assistant("reply 1"),
        Message::user("turn 2"),
    ];
    let cleared = ClearedResults::default();
    let doc = MemoryDocument::default();
    let projected = history_for_provider_request(&history, &cleared, Some(&doc));
    assert_eq!(projected.len(), history.len());
}

/// A non-empty memory document is prepended, and everything before the most
/// recent turn-opening user message is dropped (replaced by the document).
/// The tail (from the most recent user message onward) is kept verbatim.
#[test]
fn memory_projection_prepends_document_and_keeps_tail() {
    let history = vec![
        Message::user("turn 1"),
        Message::assistant("reply 1"),
        Message::user("turn 2"),
        Message::assistant("reply 2"),
    ];
    let cleared = ClearedResults::default();
    let doc = memory_doc();
    let projected = history_for_provider_request(&history, &cleared, Some(&doc));
    // [memory document] + ["turn 2", "reply 2"]
    assert_eq!(projected.len(), 3);
    // First message is the rendered memory document.
    let first = user_text(&projected[0]).expect("first message is user text");
    assert!(first.contains("Current memory document"));
    assert!(first.contains("Ship the memory feature"));
    // Tail starts at "turn 2".
    assert_eq!(user_text(&projected[1]).as_deref(), Some("turn 2"));
}

/// The memory projection composes with Tier 1 clearing: clearing is applied
/// to the full history first (correct occurrence counting), then the old
/// part is dropped and the memory document prepended. A cleared tool result
/// in the tail is still blanked.
#[test]
fn memory_projection_composes_with_clearing() {
    let history = vec![
        Message::user("turn 1"),
        tool_call_message(&[("call_1", "fs.read", serde_json::json!({}))]),
        tool_result_message("call_1", chars_for_tokens(4_000)),
        Message::user("turn 2"),
    ];
    let cleared = cleared_set(&["call_1"]);
    let doc = memory_doc();
    let projected = history_for_provider_request(&history, &cleared, Some(&doc));
    // [memory document] + ["turn 2"] — the old turn (including the cleared
    // tool result) was dropped.
    assert_eq!(projected.len(), 2);
    assert!(user_text(&projected[0])
        .unwrap()
        .contains("Current memory document"));
    assert_eq!(user_text(&projected[1]).as_deref(), Some("turn 2"));
}
