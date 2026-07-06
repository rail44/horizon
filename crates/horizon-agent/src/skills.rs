//! A minimal skill mechanism: progressive disclosure per
//! `docs/research/agent-prompting.md` Part 3.2's design-material sketch,
//! adapted for Horizon. Skills ship **embedded in the binary**
//! (`include_str!`), unlike Claude Code's filesystem-discovered
//! `SKILL.md`s -- Horizon must work regardless of the user's current
//! working directory (a skill isn't tied to a project checkout the way a
//! repository's own `AGENTS.md` is), so there is no directory to discover
//! them from at runtime.
//!
//! Three stages of disclosure, matching the design material:
//!
//! 1. **Always loaded**: nothing, for a role-less session. A session whose
//!    role names skills (`roles::RoleDefinition::skill_ids`) gets one
//!    extra prompt section (built by [`skills_prompt_section`]) listing
//!    each skill's name and description -- a location, not the content.
//! 2. **Read on demand**: the `skill.read` tool (`tools::config`) returns
//!    one skill's full body, for the agent to fetch only when its
//!    description matches the task at hand.
//! 3. **Deeper resources**: not built -- no skill here references further
//!    files/scripts yet (Part 3.2's third disclosure tier). Add it if a
//!    future skill needs it.
//!
//! `SKILL.md` format (hand-parsed below, no new dependency): a YAML-ish
//! frontmatter block delimited by `---` lines, with `name:`/`description:`
//! keys, followed by the Markdown body.

/// One embedded skill: parsed once from its `SKILL.md` source at first use
/// and cached for the process's lifetime (see [`all`]).
pub struct Skill {
    /// The identifier used both in a role's `skill_ids` and in
    /// `skill.read`'s `id` input -- taken verbatim from the `SKILL.md`
    /// frontmatter's `name:` field.
    pub name: String,
    /// The one-line summary shown in the always-loaded prompt section
    /// (`skills_prompt_section`) -- taken from `description:`.
    pub description: String,
    /// The full Markdown body, returned by `skill.read`.
    pub body: String,
}

/// This build's only skill (see `roles::CONFIG_ROLE`). Embedded so it
/// ships inside the binary -- see the module doc.
const HORIZON_CONFIG_SKILL_SOURCE: &str = include_str!("../skills/horizon-config/SKILL.md");

/// Every skill this build knows about, parsed once and cached. A `Vec`
/// (not a fixed-size array) purely because [`parse_skill_md`] returns an
/// owned `Skill`, not because the set is expected to grow large -- see the
/// module doc on this being a small, compile-time-known set.
fn all_uncached() -> Vec<Skill> {
    vec![parse_skill_md(HORIZON_CONFIG_SKILL_SOURCE)
        .expect("crates/horizon-agent/skills/horizon-config/SKILL.md must parse")]
}

/// Every skill this build knows about.
pub fn all() -> &'static [Skill] {
    static SKILLS: std::sync::OnceLock<Vec<Skill>> = std::sync::OnceLock::new();
    SKILLS.get_or_init(all_uncached)
}

/// Looks up a skill by its frontmatter `name`.
pub fn get(id: &str) -> Option<&'static Skill> {
    all().iter().find(|skill| skill.name == id)
}

/// Builds the always-loaded prompt section listing `skill_ids` (a role's
/// `RoleDefinition::skill_ids`) by name and description, plus one line
/// telling the agent to read a skill via `skill.read` before relying on
/// it -- `docs/research/agent-prompting.md` Part 3.2's "metadata now, body
/// on demand" shape. Returns `None` for an empty slice (the role-less
/// case, and any role that names no skills) so a session with nothing to
/// disclose adds no extra section at all.
pub fn skills_prompt_section(skill_ids: &[&str]) -> Option<String> {
    if skill_ids.is_empty() {
        return None;
    }
    let mut section = String::from(
        "Skills available for this session -- read one with `skill.read` before relying on \
         its contents:\n",
    );
    for id in skill_ids {
        if let Some(skill) = get(id) {
            section.push_str(&format!("- `{}` -- {}\n", skill.name, skill.description));
        }
    }
    Some(section.trim_end().to_string())
}

/// Hand-parses a `SKILL.md`'s frontmatter (`---\nname: ..\ndescription: ..\n---\n<body>`)
/// with no YAML dependency -- the format is small enough that a real
/// parser would be over-engineering for a single-digit number of skills.
/// Returns `None` for anything that doesn't match the expected shape
/// (missing delimiters, or a frontmatter missing `name`/`description`);
/// [`all_uncached`] `.expect()`s this to succeed for skills this crate
/// ships itself, so a malformed `SKILL.md` fails loudly at first use
/// (covered by this module's own test) rather than silently vanishing from
/// the registry.
fn parse_skill_md(source: &str) -> Option<Skill> {
    let after_open = source.strip_prefix("---\n")?;
    let (frontmatter, body) = after_open.split_once("\n---\n")?;

    let mut name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        let (key, value) = line.split_once(':')?;
        match key.trim() {
            "name" => name = Some(value.trim().to_string()),
            "description" => description = Some(value.trim().to_string()),
            _ => {}
        }
    }

    Some(Skill {
        name: name?,
        description: description?,
        body: body.trim_start_matches('\n').to_string(),
    })
}

/// Executes the `skill.read` tool: returns `id`'s full body, or an error
/// listing every known skill id if `id` doesn't match one. Takes the raw
/// tool input rather than an already-extracted `&str` so `tools::config`
/// (the one caller) can dispatch to this without its own argument-shape
/// checking.
pub(crate) fn execute_read(input: &serde_json::Value) -> serde_json::Value {
    let Some(id) = input.get("id").and_then(serde_json::Value::as_str) else {
        return error_output("skill.read requires an `id` string argument");
    };
    match get(id) {
        Some(skill) => serde_json::json!({
            "id": skill.name,
            "description": skill.description,
            "body": skill.body,
        }),
        None => {
            let available: Vec<&str> = all().iter().map(|skill| skill.name.as_str()).collect();
            error_output(format!(
                "unknown skill `{id}` -- available: {}",
                available.join(", ")
            ))
        }
    }
}

fn error_output(message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({ "is_error": true, "message": message.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_well_formed_skill_md() {
        let source = "---\nname: example\ndescription: An example skill.\n---\n# Body\n\nHello.\n";
        let skill = parse_skill_md(source).expect("well-formed frontmatter must parse");
        assert_eq!(skill.name, "example");
        assert_eq!(skill.description, "An example skill.");
        assert_eq!(skill.body, "# Body\n\nHello.\n");
    }

    #[test]
    fn rejects_a_skill_md_missing_the_closing_delimiter() {
        let source = "---\nname: example\ndescription: broken\n# Body\n";
        assert!(parse_skill_md(source).is_none());
    }

    #[test]
    fn rejects_a_skill_md_missing_a_required_frontmatter_key() {
        let source = "---\nname: example\n---\nBody with no description.\n";
        assert!(parse_skill_md(source).is_none());
    }

    #[test]
    fn horizon_config_skill_parses_and_is_registered() {
        let skill = get("horizon-config").expect("horizon-config skill must be registered");
        assert!(!skill.description.is_empty());
        assert!(skill.body.contains("config.toml"));
    }

    #[test]
    fn all_returns_the_same_cached_slice_on_repeated_calls() {
        assert_eq!(all().len(), all().len());
        assert!(all().iter().any(|skill| skill.name == "horizon-config"));
    }

    #[test]
    fn get_returns_none_for_an_unknown_skill_id() {
        assert!(get("does-not-exist").is_none());
    }

    #[test]
    fn skills_prompt_section_is_none_for_an_empty_skill_list() {
        assert_eq!(skills_prompt_section(&[]), None);
    }

    #[test]
    fn skills_prompt_section_lists_name_and_description_and_mentions_skill_read() {
        let section = skills_prompt_section(&["horizon-config"]).expect("must build a section");
        assert!(section.contains("horizon-config"));
        assert!(section.contains("skill.read"));
        let skill = get("horizon-config").unwrap();
        assert!(section.contains(&skill.description));
    }

    #[test]
    fn skills_prompt_section_silently_skips_an_unknown_skill_id() {
        // A role naming a skill id this build doesn't ship (e.g. stale
        // data) should not panic or blank out the whole section -- the
        // known entries still render.
        let section = skills_prompt_section(&["horizon-config", "does-not-exist"])
            .expect("must still build a section for the known entry");
        assert!(section.contains("horizon-config"));
        assert!(!section.contains("does-not-exist"));
    }

    #[test]
    fn execute_read_returns_the_body_for_a_known_id() {
        let output = execute_read(&serde_json::json!({ "id": "horizon-config" }));
        assert_eq!(output["id"], "horizon-config");
        assert!(output["body"].as_str().unwrap().contains("config.toml"));
    }

    #[test]
    fn execute_read_lists_available_ids_for_an_unknown_id() {
        let output = execute_read(&serde_json::json!({ "id": "no-such-skill" }));
        assert_eq!(output["is_error"], true);
        assert!(output["message"]
            .as_str()
            .unwrap()
            .contains("horizon-config"));
    }

    #[test]
    fn execute_read_errors_on_a_missing_id_argument() {
        let output = execute_read(&serde_json::json!({}));
        assert_eq!(output["is_error"], true);
    }
}
