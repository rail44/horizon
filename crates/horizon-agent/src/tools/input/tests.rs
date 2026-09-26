use super::*;
use serde_json::json;

pub(crate) fn samples() -> Vec<(&'static str, Value)> {
    vec![
        ("fs.read", json!({"path": "/tmp/input-example"})),
        ("fs.glob", json!({"base_path": "/tmp", "pattern": "*.rs"})),
        ("fs.grep", json!({"base_path": "/tmp", "pattern": "hello"})),
        (
            "fs.write",
            json!({"path": "/tmp/input-example", "content": "hello"}),
        ),
        (
            "fs.edit",
            json!({"edits": [{"path": "/tmp/input-example", "old_string": "hello", "new_string": "world"}]}),
        ),
        ("bash", json!({"command": "echo hello"})),
        ("config.read", json!({})),
        (
            "config.write",
            json!({"content": "[provider]\nmodel = 'example'"}),
        ),
        ("skill.read", json!({"id": "example"})),
        ("recall.search", json!({"query": "hello"})),
        ("recall.read", json!({"from_sequence": 0})),
        ("knowledge.read", json!({"id": "example"})),
        (
            "knowledge.write",
            json!({"id": "example", "description": "Example", "body": "hello", "sources": ["session:example"]}),
        ),
        (
            "memory.update",
            json!({"goal": {"op": "set", "content": "hello"}}),
        ),
        (
            "task",
            json!({"description": "Example task", "prompt": "Read the requested source"}),
        ),
        (
            "task_output",
            json!({"session_id": "12345678-1234-1234-1234-123456789abc"}),
        ),
        ("web_search", json!({"query": "hello"})),
        ("web_fetch", json!({"url": "https://example.com/docs"})),
    ]
}

#[test]
fn every_advertised_builtin_uses_its_typed_schema_and_rejects_unknown_fields() {
    let catalog = crate::tools::definitions();
    for (id, sample) in samples() {
        let schema = schema(id).expect(id);
        assert_eq!(
            catalog
                .iter()
                .find(|tool| tool.id == id)
                .unwrap()
                .input_schema,
            schema,
            "{id}"
        );
        assert_eq!(schema["additionalProperties"], false, "{id}");
        assert!(ToolInput::parse(id, &sample).is_ok(), "{id}");
        let mut extra = sample.clone();
        extra["unexpected"] = json!(true);
        assert!(
            ToolInput::parse(id, &extra)
                .unwrap_err()
                .contains("unexpected"),
            "{id}"
        );
        for wrong_shape in [Value::Null, json!([]), json!("object"), json!(42)] {
            assert!(
                ToolInput::parse(id, &wrong_shape).is_err(),
                "{id}: {wrong_shape}"
            );
        }
        if let Some(required) = schema["required"].as_array() {
            for field in required {
                let field = field.as_str().unwrap();
                let mut missing = sample.clone();
                missing.as_object_mut().unwrap().remove(field);
                assert!(ToolInput::parse(id, &missing).is_err(), "{id}.{field}");
            }
        }
    }
    assert_eq!(
        catalog
            .iter()
            .filter(|tool| schema(&tool.id).is_some())
            .count(),
        samples().len()
    );
}

#[test]
fn numeric_defaults_and_bounds_match_the_advertisement() {
    for (id, field, default, maximum) in [
        ("fs.read", "offset", 1, u64::MAX),
        ("fs.read", "limit", 500, 2000),
        ("fs.glob", "limit", 200, u64::MAX),
        ("fs.grep", "limit", 100, u64::MAX),
        ("bash", "timeout_secs", 300, 1800),
        ("recall.search", "limit", 20, 100),
        ("recall.read", "limit", 20, 100),
        ("web_search", "num_results", 5, 10),
        ("web_search", "max_characters", 2000, 4000),
        ("web_fetch", "max_characters", 20000, 50000),
    ] {
        let sample = samples()
            .into_iter()
            .find(|(name, _)| *name == id)
            .unwrap()
            .1;
        let schema = schema(id).unwrap();
        let property = &schema["properties"][field];
        assert_eq!(property["type"], "integer", "{id}.{field}");
        assert_eq!(property["default"], default, "{id}.{field}");
        assert_eq!(property["minimum"], 1, "{id}.{field}");
        assert_eq!(property["maximum"], maximum, "{id}.{field}");
        assert_eq!(
            ToolInput::parse(id, &sample).unwrap().as_json()[field],
            default,
            "{id}.{field}"
        );
        for value in [json!(1), json!(maximum)] {
            let mut input = sample.clone();
            input[field] = value;
            assert!(
                ToolInput::parse(id, &input).is_ok(),
                "{id}.{field}: {input}"
            );
        }
        let mut invalid = vec![
            json!(0),
            json!(-1),
            json!(1.5),
            json!("10"),
            Value::Null,
            json!(true),
        ];
        if maximum < u64::MAX {
            invalid.push(json!(maximum + 1));
        }
        for value in invalid {
            let mut input = sample.clone();
            input[field] = value;
            let error = ToolInput::parse(id, &input).unwrap_err();
            assert!(error.contains(field), "{id}.{field}: {error}");
        }
    }
}

#[test]
fn text_limits_count_unicode_characters_and_reject_blank_input() {
    for (id, field, maximum) in [
        ("task", "description", 200),
        ("task", "prompt", 16384),
        ("web_search", "query", 2048),
        ("web_fetch", "url", 8192),
    ] {
        let mut sample = samples()
            .into_iter()
            .find(|(name, _)| *name == id)
            .unwrap()
            .1;
        let schema = schema(id).unwrap();
        assert_eq!(
            schema["properties"][field]["maxLength"], maximum,
            "{id}.{field}"
        );
        sample[field] = json!("日".repeat(maximum));
        assert!(ToolInput::parse(id, &sample).is_ok(), "{id}.{field}");
        for value in [json!("日".repeat(maximum + 1)), json!(" \n\t"), json!(true)] {
            sample[field] = value;
            assert!(ToolInput::parse(id, &sample).is_err(), "{id}.{field}");
        }
    }
}

#[test]
fn nested_fields_and_cross_field_rules_are_checked_before_dispatch() {
    for (id, input, field) in [
        (
            "fs.edit",
            json!({"edits": [{"path": "a", "old_string": "a", "new_string": "b", "replace_all": "true"}]}),
            "replace_all",
        ),
        (
            "fs.edit",
            json!({"edits": [{"path": "a", "old_string": "a", "new_string": "b"}, {"path": "b", "old_string": "c"}]}),
            "edits[1]",
        ),
        (
            "fs.edit",
            json!({"edits": [{"path": "a", "old_string": "a", "new_string": "b", "extra": true}]}),
            "extra",
        ),
        (
            "fs.edit",
            json!({"edits": [{"path": "a", "old_string": "", "new_string": "b"}]}),
            "old_string",
        ),
        (
            "fs.edit",
            json!({"edits": [{"path": "a", "old_string": "same", "new_string": "same"}]}),
            "new_string",
        ),
        (
            "recall.search",
            json!({"scope": "all", "session_id": uuid::Uuid::nil(), "query": "hello"}),
            "session_id",
        ),
        (
            "knowledge.write",
            json!({"id": "example", "description": "Example", "body": "", "sources": ["good", 1]}),
            "sources[1]",
        ),
        (
            "knowledge.write",
            json!({"id": "example", "description": "Example", "body": "", "sources": []}),
            "sources",
        ),
        (
            "memory.update",
            json!({"goal": {"op": "set", "content": "hello", "extra": 1}}),
            "goal",
        ),
        (
            "memory.update",
            json!({"goal": {"op": "set", "content": "hello"}, "folded_log_range": {"from_seq": "1", "to_seq": 2}}),
            "from_seq",
        ),
        (
            "fs.edit",
            json!({"edits": [["/tmp/example", "old", "new", false]]}),
            "edits",
        ),
        (
            "memory.update",
            json!({"no_update": ["reason"]}),
            "no_update",
        ),
        (
            "memory.update",
            json!({"goal": {"op": "set", "content": "hello"}, "folded_log_range": [1, 2]}),
            "folded_log_range",
        ),
        ("memory.update", json!({"goal": ["set", "hello"]}), "goal"),
        (
            "memory.update",
            json!({"goal": {"op": "set", "content": "hello"}, "no_update": {"reason": "none"}}),
            "no_update",
        ),
    ] {
        assert!(
            ToolInput::parse(id, &input).unwrap_err().contains(field),
            "{id}: {input}"
        );
    }
}
