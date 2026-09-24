# Named provider configuration

`[[providers]]` is the only provider file format. `default_provider` selects
conversation sessions; `auxiliary_provider` selects one OpenAI-compatible entry
for both session titles and automatic tool-approval judgments.

```toml
# Selectors must precede table headers.
default_provider = "chat"
auxiliary_provider = "helpers"

[[providers]]
name = "chat"
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[[providers]]
name = "helpers"
kind = "openai-compatible"
api_key_env = "HELPER_API_KEY"
# base_url = "https://your-openai-compatible-endpoint/v1"
```

Only environment-variable names go in TOML; credentials stay in the environment.
`OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL` override file URLs as before.
`HORIZON_RIG_MODEL` overrides the conversation default model.
`HORIZON_AGENT_TITLE_MODEL` and `HORIZON_AGENT_JUDGE_MODEL` select auxiliary model
IDs; they must exist on the selected auxiliary endpoint. The entry's
`default_model` remains its conversation default, not an auxiliary model setting.

With no provider entries, a built-in OpenAI-compatible entry named `default`
serves both purposes. With named entries, auxiliary selection is explicit.
A missing/invalid auxiliary selection warns and leaves raw titles and human
approval active. It never borrows credentials or an endpoint from the chat
provider. Missing credentials preserve those same fallback behaviors.

Reload Config updates future title calls and new sessions. Each running session
retains its judge connection; its conversation model can be explicitly switched
using the latest accepted catalog. Title clients are pooled per accepted shell
configuration, judge clients per session. In-flight calls keep their captured
connection when configuration changes.

## One-time conversion

The old `[provider]` table is rejected, with a conversion message. Startup retains
the existing warn-and-default policy for invalid configuration; reload refuses
the file and retains the previously accepted settings. Convert before restarting
with an old file:

```sh
cargo run --locked -p horizon-config --example migrate-provider-config -- \
  /path/to/config.toml /path/to/new-config-bundle
```

The destination must not exist. The converter writes `original.toml` byte for
byte and a validated `config.toml`, without changing or installing the source.
The rewritten file preserves unrelated settings; comments and formatting remain
in the original copy. Legacy `model` becomes `default_model`; endpoint and key
conventions are preserved. The previous implementation's `open-ai-compatible`
spelling is converted to the documented `openai-compatible` spelling.

Mixed old/new configurations previously allowed different title/judge routes.
They require an explicit auxiliary provider name as a third argument. The old
table is retained as a new named entry, normally `migrated-provider`; the error
message identifies its name if a collision requires a suffix. Multiple existing
OpenAI-compatible entries also require a selection unless one is already set.
No destination is written until the choice validates.

Review the two files before installing the converted one. The old
`reload-session-runtime` keybinding is converted to `reload-agent-runtime`.
`new-config-agent` has no equivalent single command ID: remove that binding and
use `horizon new-agent --role config`; the converter refuses to change its meaning.

Real user configuration is not modified by repository integration. Agent history
also needs the separate [format-v2 conversion](agent-history-format-v2.md) before
the agent wire-v23 restart.
