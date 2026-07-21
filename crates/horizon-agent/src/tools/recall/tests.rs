use super::*;
use crate::config::AgentToolsConfig;
use crate::contract::{
    Event, Message, MessageRole, ToolCallId, ToolCallRequest, ToolCallResult, TurnEndReason,
};
use crate::persistence::projection::duckdb::{AppendEvent, DuckdbStoreHandle, Store};
use crate::tools::state::RecallContext;

/// Builds a fresh file-backed DuckDB projection at a throwaway path, seeded
/// with `sessions` (each a `SessionId` and its committed messages), and a
/// `ToolSessionState` whose recall context shares that same store (never a
/// fresh `Store::open` -- see this module's doc comment) with
/// `own_session_id` as "this session". Returns the tool state and the
/// path, so a test can clean the file up afterward.
fn tool_state_with_seeded_store(
    own_session_id: SessionId,
    sessions: Vec<(SessionId, Vec<&str>)>,
) -> (ToolSessionState, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "horizon-agent-recall-tool-{}.duckdb",
        uuid::Uuid::new_v4()
    ));
    let store = Store::open(&path).expect("open duckdb store");
    for (session_id, texts) in sessions {
        let events = texts
            .into_iter()
            .map(|text| {
                Event::MessageCommitted(Message {
                    role: MessageRole::User,
                    text: text.to_string(),
                })
            })
            .collect::<Vec<_>>();
        store
            .append_events(session_id, None, events)
            .expect("seed session");
    }

    let recall = RecallContext {
        session_id: Some(own_session_id),
        store: Some(DuckdbStoreHandle::new(store)),
    };
    let tool_state = ToolSessionState::for_current_dir(AgentToolsConfig::default(), recall);
    (tool_state, path)
}

#[test]
fn search_defaults_to_this_session_and_flags_it_as_own() {
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let (tool_state, path) = tool_state_with_seeded_store(
        session_a,
        vec![
            (session_a, vec!["widget in session a"]),
            (session_b, vec!["widget in session b"]),
        ],
    );

    let output = execute_auto(&tool_state, "recall.search", &json!({ "query": "widget" }))
        .expect("recall.search handled");
    assert_eq!(
        output["total"], 1,
        "default scope must be this session only"
    );
    let hits = output["hits"].as_array().expect("hits array");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["own_session"], true);
    assert!(hits[0]["snippet"]
        .as_str()
        .unwrap()
        .contains("widget in session a"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn search_scope_all_crosses_sessions_and_flags_own_session_correctly() {
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let (tool_state, path) = tool_state_with_seeded_store(
        session_a,
        vec![
            (session_a, vec!["widget in session a"]),
            (session_b, vec!["widget in session b"]),
        ],
    );

    let output = execute_auto(
        &tool_state,
        "recall.search",
        &json!({ "query": "widget", "scope": "all" }),
    )
    .expect("recall.search handled");
    assert_eq!(output["total"], 2);
    let hits = output["hits"].as_array().expect("hits array");
    assert_eq!(hits.len(), 2);
    let own_flags: Vec<bool> = hits
        .iter()
        .map(|hit| hit["own_session"].as_bool().unwrap())
        .collect();
    assert_eq!(
        own_flags.iter().filter(|flag| **flag).count(),
        1,
        "exactly one hit must belong to this session"
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn search_without_a_configured_db_path_errors_clearly() {
    let tool_state =
        ToolSessionState::for_current_dir(AgentToolsConfig::default(), RecallContext::default());

    let output = execute_auto(
        &tool_state,
        "recall.search",
        &json!({ "query": "anything" }),
    )
    .expect("recall.search handled");
    assert_eq!(output["is_error"], true);
}

#[test]
fn search_session_scope_without_a_session_id_errors_instead_of_falling_back_to_all() {
    let path = std::env::temp_dir().join(format!(
        "horizon-agent-recall-tool-noscope-{}.duckdb",
        uuid::Uuid::new_v4()
    ));
    let store = Store::open(&path).expect("open duckdb store");
    store
        .append_events(
            SessionId::new(),
            None,
            [Event::MessageCommitted(Message {
                role: MessageRole::User,
                text: "widget".to_string(),
            })],
        )
        .expect("seed session");

    // No session id in context (shouldn't happen in production, but must
    // fail loudly rather than silently searching everything).
    let recall = RecallContext {
        session_id: None,
        store: Some(DuckdbStoreHandle::new(store)),
    };
    let tool_state = ToolSessionState::for_current_dir(AgentToolsConfig::default(), recall);

    let output = execute_auto(&tool_state, "recall.search", &json!({ "query": "widget" }))
        .expect("recall.search handled");
    assert_eq!(output["is_error"], true);

    let _ = std::fs::remove_file(path);
}

#[test]
fn read_caps_total_text_output_and_notes_truncation() {
    let session_id = SessionId::new();
    // Five messages, each long enough that the SQL layer's own per-row
    // bound (4000 chars) still leaves the *total* across all five well
    // past `READ_TOTAL_CHAR_CAP` (16k) -- exercising the cap without
    // needing any single message to exceed the SQL-layer bound itself.
    let long_texts: Vec<String> = (0..5)
        .map(|index| format!("entry {index} {}", "x".repeat(4_500)))
        .collect();
    let (tool_state, path) = tool_state_with_seeded_store(
        session_id,
        vec![(session_id, long_texts.iter().map(String::as_str).collect())],
    );

    let output = execute_auto(
        &tool_state,
        "recall.read",
        &json!({ "from_sequence": 0, "limit": 100 }),
    )
    .expect("recall.read handled");

    let entries = output["entries"].as_array().expect("entries array");
    let total_chars: usize = entries
        .iter()
        .map(|entry| entry["text"].as_str().unwrap().chars().count())
        .sum();
    assert!(
        total_chars <= READ_TOTAL_CHAR_CAP,
        "total output text must respect the cap, got {total_chars}"
    );
    assert!(
        entries.len() < long_texts.len(),
        "the cap must stop before every seeded entry is included"
    );
    assert!(output["note"].as_str().unwrap().contains("truncated"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn read_without_a_configured_db_path_errors_clearly() {
    let tool_state =
        ToolSessionState::for_current_dir(AgentToolsConfig::default(), RecallContext::default());

    let output = execute_auto(&tool_state, "recall.read", &json!({ "from_sequence": 0 }))
        .expect("recall.read handled");
    assert_eq!(output["is_error"], true);
}

#[test]
fn snippet_around_match_centers_on_the_first_match_with_ellipses() {
    let text = format!("{}NEEDLE{}", "a".repeat(500), "b".repeat(500));
    let snippet = snippet_around_match(&text, "needle");

    assert!(
        snippet.starts_with("..."),
        "left side must be trimmed: {snippet}"
    );
    assert!(
        snippet.ends_with("..."),
        "right side must be trimmed: {snippet}"
    );
    assert!(
        snippet.to_lowercase().contains("needle"),
        "snippet must contain the match: {snippet}"
    );
    // Roughly `2 * SNIPPET_RADIUS_CHARS` plus the match itself plus the two
    // "..." markers -- generous bounds so this isn't brittle to the exact
    // constant.
    assert!(snippet.chars().count() < 300);
}

#[test]
fn snippet_around_match_has_no_ellipses_when_the_match_is_near_the_edges() {
    let text = "needle at the very start of a short string";
    let snippet = snippet_around_match(text, "needle");
    assert!(!snippet.starts_with("..."));
    assert_eq!(snippet, text);
}

/// Appends a message, then a `TurnEnded(reason)`, both under `turn_id` --
/// unlike `tool_state_with_seeded_store`'s helper (which uses
/// `Store::append_events` and always leaves `turn_id: None`), this goes
/// through `Store::append_event` directly so the seeded events actually
/// belong to a turn `agent_turns` can join against.
fn seed_turn(
    store: &Store,
    session_id: SessionId,
    turn_id: &str,
    message_text: &str,
    reason: TurnEndReason,
) {
    store
        .append_event(AppendEvent {
            session_id,
            turn_id: Some(turn_id.to_string()),
            provider_id: None,
            role_id: None,
            event: Event::MessageCommitted(Message {
                role: MessageRole::User,
                text: message_text.to_string(),
            }),
            provider_payload: None,
        })
        .expect("append seeded message");
    store
        .append_event(AppendEvent {
            session_id,
            turn_id: Some(turn_id.to_string()),
            provider_id: None,
            role_id: None,
            event: Event::TurnEnded(reason),
            provider_payload: None,
        })
        .expect("append turn ended");
}

#[test]
fn search_filters_by_turn_outcome() {
    let session_id = SessionId::new();
    let path = std::env::temp_dir().join(format!(
        "horizon-agent-recall-turn-outcome-{}.duckdb",
        uuid::Uuid::new_v4()
    ));
    let store = Store::open(&path).expect("open duckdb store");
    seed_turn(
        &store,
        session_id,
        "turn-completed",
        "widget alpha",
        TurnEndReason::Completed,
    );
    seed_turn(
        &store,
        session_id,
        "turn-halted",
        "widget beta",
        TurnEndReason::Halted,
    );

    let recall = RecallContext {
        session_id: Some(session_id),
        store: Some(DuckdbStoreHandle::new(store)),
    };
    let tool_state = ToolSessionState::for_current_dir(AgentToolsConfig::default(), recall);

    let output = execute_auto(
        &tool_state,
        "recall.search",
        &json!({ "query": "widget", "turn_outcome": "halted" }),
    )
    .expect("recall.search handled");

    assert_eq!(output["total"], 1, "only the halted turn's hit must match");
    let hits = output["hits"].as_array().expect("hits array");
    assert_eq!(hits.len(), 1);
    assert!(hits[0]["snippet"].as_str().unwrap().contains("widget beta"));
    assert_eq!(hits[0]["turn_outcome"], "halted");

    let _ = std::fs::remove_file(path);
}

#[test]
fn search_hits_carry_is_error_and_turn_outcome_labels() {
    let session_id = SessionId::new();
    let path = std::env::temp_dir().join(format!(
        "horizon-agent-recall-labels-{}.duckdb",
        uuid::Uuid::new_v4()
    ));
    let store = Store::open(&path).expect("open duckdb store");
    let call_id = ToolCallId("call-1".to_string());

    store
        .append_event(AppendEvent {
            session_id,
            turn_id: Some("turn-1".to_string()),
            provider_id: None,
            role_id: None,
            event: Event::ToolCallRequested(ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "fs.read".to_string(),
                input: serde_json::json!({ "path": "widget.txt" }).into(),
            }),
            provider_payload: None,
        })
        .expect("append tool call");
    store
        .append_event(AppendEvent {
            session_id,
            turn_id: Some("turn-1".to_string()),
            provider_id: None,
            role_id: None,
            event: Event::ToolCallFinished(ToolCallResult::new(
                call_id,
                serde_json::json!({ "is_error": true, "message": "widget not found" }),
            )),
            provider_payload: None,
        })
        .expect("append tool result");
    store
        .append_event(AppendEvent {
            session_id,
            turn_id: Some("turn-1".to_string()),
            provider_id: None,
            role_id: None,
            event: Event::TurnEnded(TurnEndReason::Failed),
            provider_payload: None,
        })
        .expect("append turn ended");

    let recall = RecallContext {
        session_id: Some(session_id),
        store: Some(DuckdbStoreHandle::new(store)),
    };
    let tool_state = ToolSessionState::for_current_dir(AgentToolsConfig::default(), recall);

    let output = execute_auto(&tool_state, "recall.search", &json!({ "query": "widget" }))
        .expect("recall.search handled");
    let hits = output["hits"].as_array().expect("hits array");

    let tool_result_hit = hits
        .iter()
        .find(|hit| hit["kind"] == "tool_result")
        .expect("tool_result hit present");
    assert_eq!(tool_result_hit["is_error"], true);
    assert_eq!(tool_result_hit["turn_outcome"], "failed");

    let tool_call_hit = hits
        .iter()
        .find(|hit| hit["kind"] == "tool_call")
        .expect("tool_call hit present");
    assert_eq!(
        tool_call_hit["is_error"],
        Value::Null,
        "a tool_call hit has no is_error label"
    );
    assert_eq!(tool_call_hit["turn_outcome"], "failed");

    let _ = std::fs::remove_file(path);
}

#[test]
fn search_without_query_but_with_turn_outcome_lists_matches() {
    let session_id = SessionId::new();
    let path = std::env::temp_dir().join(format!(
        "horizon-agent-recall-listing-mode-{}.duckdb",
        uuid::Uuid::new_v4()
    ));
    let store = Store::open(&path).expect("open duckdb store");
    seed_turn(
        &store,
        session_id,
        "turn-completed",
        "widget alpha",
        TurnEndReason::Completed,
    );
    seed_turn(
        &store,
        session_id,
        "turn-halted",
        "widget beta",
        TurnEndReason::Halted,
    );

    let recall = RecallContext {
        session_id: Some(session_id),
        store: Some(DuckdbStoreHandle::new(store)),
    };
    let tool_state = ToolSessionState::for_current_dir(AgentToolsConfig::default(), recall);

    let output = execute_auto(
        &tool_state,
        "recall.search",
        &json!({ "turn_outcome": "halted" }),
    )
    .expect("recall.search handled");

    assert_eq!(
        output["total"], 1,
        "listing mode must find only the halted turn's hit with no query"
    );
    let hits = output["hits"].as_array().expect("hits array");
    assert_eq!(hits.len(), 1);
    assert!(hits[0]["snippet"].as_str().unwrap().contains("widget beta"));
    assert_eq!(hits[0]["turn_outcome"], "halted");

    let _ = std::fs::remove_file(path);
}

#[test]
fn search_without_query_and_without_turn_outcome_errors_clearly() {
    let session_id = SessionId::new();
    let (tool_state, path) =
        tool_state_with_seeded_store(session_id, vec![(session_id, vec!["widget"])]);

    let output =
        execute_auto(&tool_state, "recall.search", &json!({})).expect("recall.search handled");
    assert_eq!(output["is_error"], true);
    assert!(output["message"]
        .as_str()
        .unwrap()
        .contains("query` or a `turn_outcome`"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn search_rejects_an_unknown_turn_outcome_value() {
    let session_id = SessionId::new();
    let (tool_state, path) =
        tool_state_with_seeded_store(session_id, vec![(session_id, vec!["widget"])]);

    let output = execute_auto(
        &tool_state,
        "recall.search",
        &json!({ "query": "widget", "turn_outcome": "bogus" }),
    )
    .expect("recall.search handled");
    assert_eq!(output["is_error"], true);
    assert!(output["message"].as_str().unwrap().contains("turn_outcome"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn read_entries_carry_is_error_on_tool_results() {
    let session_id = SessionId::new();
    let path = std::env::temp_dir().join(format!(
        "horizon-agent-recall-read-is-error-{}.duckdb",
        uuid::Uuid::new_v4()
    ));
    let store = Store::open(&path).expect("open duckdb store");
    let call_id = ToolCallId("call-1".to_string());
    store
        .append_events(
            session_id,
            None,
            [
                Event::ToolCallRequested(ToolCallRequest {
                    call_id: call_id.clone(),
                    tool_id: "fs.read".to_string(),
                    input: serde_json::json!({}).into(),
                }),
                Event::ToolCallFinished(ToolCallResult::new(
                    call_id,
                    serde_json::json!({ "is_error": true, "message": "nope" }),
                )),
            ],
        )
        .expect("seed session");

    let recall = RecallContext {
        session_id: Some(session_id),
        store: Some(DuckdbStoreHandle::new(store)),
    };
    let tool_state = ToolSessionState::for_current_dir(AgentToolsConfig::default(), recall);

    let output = execute_auto(&tool_state, "recall.read", &json!({ "from_sequence": 0 }))
        .expect("recall.read handled");
    let entries = output["entries"].as_array().expect("entries array");
    let tool_result_entry = entries
        .iter()
        .find(|entry| entry["kind"] == "tool_result")
        .expect("tool_result entry present");
    assert_eq!(tool_result_entry["is_error"], true);

    let tool_call_entry = entries
        .iter()
        .find(|entry| entry["kind"] == "tool_call")
        .expect("tool_call entry present");
    assert_eq!(tool_call_entry["is_error"], Value::Null);

    let _ = std::fs::remove_file(path);
}

#[test]
fn catalog_lists_recall_tools_as_auto_allow_read() {
    let definitions = crate::tools::definitions();
    for id in ["recall.search", "recall.read"] {
        let definition = definitions
            .iter()
            .find(|definition| definition.id == id)
            .unwrap_or_else(|| panic!("catalog must list `{id}`"));
        assert_eq!(
            definition.permission,
            crate::contract::ToolPermission::AutoAllowRead
        );
        assert_eq!(
            crate::tools::permission_for_tool(id),
            Some(crate::contract::ToolPermission::AutoAllowRead)
        );
    }
}
