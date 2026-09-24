//! One-time conversion; production configuration never interprets legacy fields.

use std::path::Path;
use toml::{Table, Value};

fn convert(source: &str, auxiliary_choice: Option<&str>) -> Result<String, String> {
    let mut root: Table =
        toml::from_str(source).map_err(|error| format!("invalid TOML: {error}"))?;
    let legacy = root.remove("provider");
    let mut entries = match root.remove("providers") {
        Some(Value::Array(entries)) => entries,
        None => Vec::new(),
        _ => return Err("providers must be an array of tables".into()),
    };
    let mixed = legacy.is_some() && !entries.is_empty();
    // Earlier serde kebab-case spelling differed from the documented value.
    for entry in &mut entries {
        if entry.get("kind").and_then(Value::as_str) == Some("open-ai-compatible") {
            entry
                .as_table_mut()
                .unwrap()
                .insert("kind".into(), Value::String("openai-compatible".into()));
        }
    }
    let mut migrated_name = None;
    if let Some(legacy) = legacy {
        let mut old = legacy
            .as_table()
            .cloned()
            .ok_or("provider must be a table")?;
        if old
            .keys()
            .any(|key| !["model", "base_url"].contains(&key.as_str()))
        {
            return Err(
                "[provider] contains unsupported keys; review them before conversion".into(),
            );
        }
        let mut name = if entries.is_empty() {
            "default"
        } else {
            "migrated-provider"
        }
        .to_owned();
        while entries
            .iter()
            .any(|entry| entry.get("name").and_then(Value::as_str) == Some(&name))
        {
            name.push_str("-old");
        }
        let mut entry = Table::new();
        entry.insert("name".into(), Value::String(name.clone()));
        entry.insert("kind".into(), Value::String("openai-compatible".into()));
        entry.insert("api_key_env".into(), Value::String("OPENAI_API_KEY".into()));
        for (from, to) in [("model", "default_model"), ("base_url", "base_url")] {
            if let Some(value) = old.remove(from) {
                if !value.is_str() {
                    return Err(format!("[provider].{from} must be a string"));
                }
                entry.insert(to.into(), value);
            }
        }
        if entries.is_empty() {
            root.insert("default_provider".into(), Value::String(name.clone()));
        }
        entries.push(Value::Table(entry));
        migrated_name = Some(name);
    }
    if !entries.is_empty() {
        root.insert("providers".into(), Value::Array(entries));
    }
    if let Some(choice) = auxiliary_choice {
        root.insert("auxiliary_provider".into(), Value::String(choice.into()));
    } else if !root.contains_key("auxiliary_provider") {
        if mixed {
            return Err(format!(
                "mixed provider formats had separate auxiliary routing; choose a named auxiliary provider explicitly (the old table becomes {:?})",
                migrated_name.as_deref().unwrap(),
            ));
        }
        let raw: horizon_config::RawConfig = Value::Table(root.clone())
            .try_into()
            .map_err(|e: toml::de::Error| e.to_string())?;
        let candidates = raw
            .resolved_providers()
            .providers
            .into_iter()
            .filter(|entry| entry.kind == horizon_config::RawProviderKind::OpenAiCompatible)
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return Err("choose exactly one named OpenAI-compatible auxiliary provider".into());
        }
        root.insert(
            "auxiliary_provider".into(),
            Value::String(candidates[0].name.clone()),
        );
    }
    if let Some(Value::Table(bindings)) = root.get_mut("keybindings") {
        for (_, command) in bindings.iter_mut() {
            match command.as_str() {
                Some("reload-session-runtime") => *command = Value::String("reload-agent-runtime".into()),
                Some("new-config-agent") => return Err("new-config-agent has no equivalent keybinding command; remove that binding and use `horizon new-agent --role config`".into()),
                _ => {}
            }
        }
    }
    let output = toml::to_string_pretty(&root).map_err(|error| error.to_string())?;
    let raw = horizon_config::parse(&output)?;
    raw.resolved_auxiliary_provider()?;
    let warnings = horizon_config::provider_config_warnings(&raw);
    if !warnings.is_empty() {
        return Err(warnings.join("; "));
    }
    Ok(output)
}

fn write_bundle(
    source: &Path,
    destination: &Path,
    auxiliary: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let original = std::fs::read_to_string(source)?;
    let converted = convert(&original, auxiliary)?;
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(destination)?;
    std::fs::write(destination.join("original.toml"), &original)?;
    std::fs::write(destination.join("config.toml"), converted)?;
    if std::fs::read_to_string(source)? != original {
        return Err("source changed during conversion; discard the bundle and try again".into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !(2..=3).contains(&args.len()) {
        return Err(
            "usage: migrate-provider-config SOURCE NEW_DIRECTORY [AUXILIARY_PROVIDER]".into(),
        );
    }
    write_bundle(
        Path::new(&args[0]),
        Path::new(&args[1]),
        args.get(2).map(String::as_str),
    )?;
    println!(
        "Created {}/config.toml and original.toml. Review before installation; source unchanged.",
        args[1]
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_provider_and_unrelated_settings_survive_conversion() {
        let source = "[provider]\nmodel = 'chat'\nbase_url = 'https://old.invalid/v1'\n[terminal]\nfont_size = 17\n[keybindings]\n'x' = 'reload-session-runtime'\n";
        let converted = convert(source, None).unwrap();
        let raw = horizon_config::parse(&converted).unwrap();
        let auxiliary = raw.resolved_auxiliary_provider().unwrap();
        assert_eq!(auxiliary.name, "default");
        assert_eq!(
            auxiliary.base_url.as_deref(),
            Some("https://old.invalid/v1")
        );
        assert_eq!(raw.providers[0].default_model.as_deref(), Some("chat"));
        assert_eq!(raw.terminal.font_size, Some(17.0));
        assert_eq!(raw.keybindings["x"], "reload-agent-runtime");
        assert_eq!(convert(&converted, None).unwrap(), converted);
    }

    #[test]
    fn mixed_configuration_requires_a_choice_and_preserves_both_endpoints() {
        let source = "[provider]\nbase_url = 'https://title.invalid/v1'\n[[providers]]\nname = 'chat'\nkind = 'anthropic'\n";
        assert!(convert(source, None).unwrap_err().contains("choose"));
        let output = convert(source, Some("migrated-provider")).unwrap();
        let raw = horizon_config::parse(&output).unwrap();
        assert_eq!(raw.resolved_providers().default_name, "chat");
        assert_eq!(
            raw.resolved_auxiliary_provider()
                .unwrap()
                .base_url
                .as_deref(),
            Some("https://title.invalid/v1")
        );
        assert!(convert(source, Some("chat")).is_err());
    }

    #[test]
    fn ambiguous_or_invalid_settings_are_not_silently_rewritten() {
        assert!(convert("[[providers]]\nname='a'\n[[providers]]\nname='b'", None).is_err());
        assert!(convert("[provider]\napi_key='secret'", None).is_err());
        assert!(convert("[keybindings]\n'x'='new-config-agent'", None).is_err());
        assert!(convert("[provider]\nmodel=123", None).is_err());
    }

    #[test]
    fn converts_the_previous_serde_spelling_to_the_documented_provider_kind() {
        let converted = convert(
            "[[providers]]\nname='helper'\nkind='open-ai-compatible'",
            None,
        )
        .unwrap();
        let raw = horizon_config::parse(&converted).unwrap();
        assert_eq!(
            raw.resolved_auxiliary_provider().unwrap().kind,
            horizon_config::RawProviderKind::OpenAiCompatible
        );
        assert!(!converted.contains("open-ai-compatible"));
    }

    #[test]
    fn bundle_preserves_original_bytes_and_refuses_overwrite() {
        let root =
            std::env::temp_dir().join(format!("horizon-config-convert-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("source.toml");
        let original = "# preserve this comment\n[provider]\nmodel = 'chat'\n";
        std::fs::write(&source, original).unwrap();
        let output = root.join("output");
        write_bundle(&source, &output, None).unwrap();
        assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(output.join("original.toml")).unwrap(),
            original
        );
        assert!(write_bundle(&source, &output, None).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
