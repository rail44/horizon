//! Derives a tool description's field legend from its output type.
//!
//! A tool's catalog description is hand-written prose, but which fields its
//! result carries and what they mean is a property of the output type. Leaving
//! that half of the description hand-written too let the two drift: the
//! `recall_sessions` description said only "bounded error/approval counts"
//! while the SQL produced four unrelated aggregates, and the one accurate
//! sentence lived in a doc comment the model never sees.
//!
//! The legend removes that failure mode by generating the field half from the
//! output type's `JsonSchema` derivation, where a field's doc comment becomes
//! its schema `description`. A tool whose output type gains a field, or whose
//! field loses its meaning, changes the legend without anyone editing prose.
//!
//! Prose that a schema cannot express -- how to choose between tools, what a
//! result means for the next step -- stays hand-written in the catalog and is
//! placed before the legend.

use schemars::JsonSchema;
use serde_json::Value;

/// Renders `type_name`'s output fields as a one-line-per-field legend.
///
/// Each line is `field — description`, with the description taken from the
/// field's doc comment. A field without a doc comment renders as `field` alone;
/// the catalog tests reject that state, this function only reports it.
pub(crate) fn legend<T: JsonSchema>(type_name: &str) -> String {
    legend_from_schema(type_name, &field_schema::<T>())
}

/// [`legend`] from an output type's schema already generated, for a tool
/// registered from a crate that does not depend on this one
/// (`tools::external`). The schema should be generated as [`field_schema`]
/// generates one: draft 2020-12 with subschemas inlined, so fields sit at
/// the top level.
pub(crate) fn legend_from_schema(type_name: &str, schema: &Value) -> String {
    let properties = properties_of(schema);

    let mut lines = Vec::with_capacity(properties.len());
    for (name, property) in properties {
        match property.get("description").and_then(Value::as_str) {
            Some(description) => lines.push(format!("{name} — {}", unwrap_lines(description))),
            None => lines.push(name),
        }
    }

    if lines.is_empty() {
        return String::new();
    }
    format!("{type_name} fields:\n{}", lines.join("\n"))
}

/// Collects a schema's fields, looking through the shapes a tagged enum makes.
///
/// A plain struct carries `properties` at the top. A `#[serde(tag = ...)]`
/// enum instead carries `oneOf` of per-variant objects that each spell the tag
/// and their own fields, so a legend built only from the top level would come
/// out empty — and an empty legend silently vanishes from the description
/// rather than failing loudly.
fn properties_of(schema: &Value) -> Vec<(String, &Value)> {
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        return properties
            .iter()
            .map(|(name, property)| (name.clone(), property))
            .collect();
    }

    let Some(variants) = schema.get("oneOf").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen: Vec<String> = Vec::new();
    let mut fields = Vec::new();
    for variant in variants {
        let Some(properties) = variant.get("properties").and_then(Value::as_object) else {
            continue;
        };
        for (name, property) in properties {
            // A tagged enum repeats its tag in every variant; list it once.
            if seen.iter().any(|existing| existing == name) {
                continue;
            }
            seen.push(name.clone());
            fields.push((name.clone(), property));
        }
    }
    fields
}

/// The same generator settings the tool *input* schemas use
/// (`tools::input::schema`), so both halves of a tool definition describe the
/// same JSON shape. Draft 2020-12 rather than draft-07 because this schema is
/// never sent to a provider: it is read here and dropped.
fn field_schema<T: JsonSchema>() -> Value {
    let settings = schemars::generate::SchemaSettings::draft2020_12().with(|settings| {
        settings.inline_subschemas = true;
    });
    let mut schema = settings.into_generator().into_root_schema_for::<T>();
    schema.remove("$schema");
    schema.remove("title");
    schema.to_value()
}

/// Reflows a doc comment into one line.
///
/// A doc comment is wrapped at the source's line width, and the schema keeps
/// those newlines verbatim. A legend that preserved them would break a
/// sentence mid-phrase wherever the author's editor wrapped it. A blank line is
/// a deliberate paragraph break, so it ends the phrase instead.
fn unwrap_lines(description: &str) -> String {
    let mut out = String::with_capacity(description.len());
    let mut pending_break = false;
    for line in description.lines() {
        let line = line.trim();
        if line.is_empty() {
            pending_break = true;
            continue;
        }
        if out.is_empty() {
            out.push_str(line);
        } else if pending_break {
            if !out.ends_with(['.', '!', '?']) {
                out.push('.');
            }
            out.push(' ');
            out.push_str(line);
        } else {
            out.push(' ');
            out.push_str(line);
        }
        pending_break = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Doc comments become descriptions; a field without one renders bare.
    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Sample {
        /// Counts containment refusals.
        denials: usize,
        undocumented: bool,
    }

    #[test]
    fn renders_one_line_per_field_with_its_doc_comment() {
        let legend = legend::<Sample>("Sample");
        assert_eq!(
            legend,
            "Sample fields:\ndenials — Counts containment refusals.\nundocumented"
        );
    }

    /// A wrapped doc comment is reflowed, so a sentence does not break where
    /// the author's editor happened to wrap it.
    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Wrapped {
        /// Counts refusals the sandbox reported from the kernel,
        /// never ones a command merely printed.
        denials: usize,
    }

    #[test]
    fn reflows_a_wrapped_doc_comment_onto_one_line() {
        assert_eq!(
            legend::<Wrapped>("Wrapped"),
            "Wrapped fields:\ndenials — Counts refusals the sandbox reported from the \
             kernel, never ones a command merely printed."
        );
    }

    /// A blank line in a doc comment is a deliberate paragraph break. The
    /// legend is one line per field, so the break must still separate the two
    /// sentences rather than run them together.
    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Paragraphs {
        /// Where it ended.
        ///
        /// Absolute, and only on a change.
        cwd: Option<String>,
    }

    #[test]
    fn a_paragraph_break_stays_a_break() {
        assert_eq!(
            legend::<Paragraphs>("Paragraphs"),
            "Paragraphs fields:\ncwd — Where it ended. Absolute, and only on a change."
        );
    }

    /// A type with no object properties contributes no legend, so a tool whose
    /// result is a bare scalar keeps its hand-written prose alone.
    #[test]
    fn renders_nothing_for_a_non_object_schema() {
        assert_eq!(legend::<String>("Text"), "");
    }

    /// A tagged enum carries its fields inside `oneOf`, one copy per variant,
    /// and its tag once. A legend that only read the top level would come out
    /// empty and then vanish from the description without failing.
    #[derive(JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "status", rename_all = "snake_case")]
    enum Tagged {
        Started {
            /// The child session's id.
            session_id: String,
        },
        Finished {
            /// The child session's id.
            session_id: String,
            /// What the child found.
            report: Option<String>,
        },
    }

    #[test]
    fn reads_the_fields_of_a_tagged_enum_once_each() {
        let legend = legend::<Tagged>("Tagged");
        assert!(
            legend.starts_with("Tagged fields:\n"),
            "a tagged enum must still produce a legend, got {legend:?}"
        );
        assert_eq!(
            legend.matches("The child session's id.").count(),
            1,
            "a field repeated across variants is listed once"
        );
        assert!(legend.contains("report — What the child found."));
    }
}
