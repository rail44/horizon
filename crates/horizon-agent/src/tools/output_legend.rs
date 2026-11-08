//! Derives a tool description's field legend from its output type.
//!
//! A tool's catalog description is hand-written prose, but which fields its
//! result carries and what they mean is a property of the output type. Leaving
//! that half of the description hand-written too let the two drift: the
//! `recall.sessions` description said only "bounded error/approval counts"
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
    let schema = field_schema::<T>();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return String::new();
    };

    let mut lines = Vec::with_capacity(properties.len());
    for (name, property) in properties {
        match property.get("description").and_then(Value::as_str) {
            Some(description) => lines.push(format!("{name} — {}", unwrap_lines(description))),
            None => lines.push(name.clone()),
        }
    }

    if lines.is_empty() {
        return String::new();
    }
    format!("{type_name} fields:\n{}", lines.join("\n"))
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
    let mut at_break = false;
    for line in description.lines() {
        let line = line.trim();
        if line.is_empty() {
            at_break = true;
            continue;
        }
        if !out.is_empty() && !at_break {
            out.push(' ');
        }
        out.push_str(line);
        at_break = false;
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

    /// A type with no object properties contributes no legend, so a tool whose
    /// result is a bare scalar keeps its hand-written prose alone.
    #[test]
    fn renders_nothing_for_a_non_object_schema() {
        assert_eq!(legend::<String>("Text"), "");
    }
}
