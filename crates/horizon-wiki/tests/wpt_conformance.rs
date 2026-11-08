//! The annotations this crate writes against the W3C Web Annotation test
//! suite's MUST assertions (web-platform-tests `annotation-model`, vendored
//! in `tests/wpt` with its 3-Clause BSD license): every assertion listed in
//! `annotationMusts.test`, each a draft-04 JSON Schema whose relative `id`
//! and `$ref`s resolve among the vendored `definitions/`.

use std::path::{Path, PathBuf};

use horizon_wiki::annotation::{markdown_body, Agent, Annotation};
use horizon_wiki::Store;
use serde_json::Value;
use uuid::Uuid;

/// Any base works: the suite names its schemas relative to one directory.
const BASE: &str = "https://wpt.invalid/annotation-model/";

fn wpt() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("wpt")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn sample_annotations() -> Vec<Value> {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(dir.path());
    store
        .write_page(
            "sandbox",
            "---\ntitle: Sandbox\n---\n# Sandbox\n\nThe helper runs **nono** first.\n",
        )
        .unwrap();
    let owner = Agent::person(Uuid::new_v4(), "owner");
    let mut body = markdown_body("なぜ最初に？");
    body.language = Some("ja".into());
    let (page, question) = store.ask("sandbox", 0..7, owner, body).unwrap();
    let reply = store
        .reply(
            page,
            &question.id,
            Agent::session(Uuid::new_v4()),
            markdown_body("Because it **must** run first."),
        )
        .unwrap();
    [question, reply]
        .iter()
        .map(|annotation: &Annotation| serde_json::to_value(annotation).unwrap())
        .collect()
}

/// Every MUST assertion's failures for `sample`, as messages.
fn must_failures(sample: &Value) -> Vec<String> {
    let root = wpt();
    let mut registry = jsonschema::Registry::new();
    for entry in std::fs::read_dir(root.join("definitions")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        registry = registry
            .add(format!("{BASE}{name}"), read_json(&path))
            .unwrap();
    }
    let registry = registry.prepare().unwrap();

    let musts = read_json(&root.join("annotations").join("annotationMusts.test"));
    let assertions = musts["assertions"].as_array().unwrap();
    assert_eq!(assertions.len(), 54, "the MUST list changed upstream");

    let mut failures = Vec::new();
    for assertion in assertions {
        let assertion = assertion.as_str().unwrap();
        let schema = read_json(&root.join(assertion));
        assert_eq!(schema["expectedResult"], "valid", "{assertion}");
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft4)
            .with_base_uri(BASE)
            .with_registry(&registry)
            .should_validate_formats(true)
            .build(&schema)
            .unwrap_or_else(|error| panic!("{assertion}: {error}"));
        if let Err(error) = validator.validate(sample) {
            failures.push(format!("{assertion} ({}): {error}", sample["motivation"]));
        }
    }
    failures
}

#[test]
fn generated_annotations_meet_every_must_assertion() {
    for sample in sample_annotations() {
        let failures = must_failures(&sample);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

#[test]
fn a_broken_annotation_is_caught() {
    // The harness must be able to fail, formats included.
    let mut question = sample_annotations().remove(0);
    question.as_object_mut().unwrap().remove("@context");
    question["target"]["state"]["sourceDate"] = "yesterday".into();
    let failures = must_failures(&question).join("\n");
    assert!(
        failures.contains("3.1-annotationContextValidated"),
        "{failures}"
    );
    assert!(failures.contains("4.3.1-timeStateValid"), "{failures}");
}
