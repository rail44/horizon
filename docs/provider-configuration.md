# Provider and model configuration

`[[providers]]` entries describe endpoints and credentials only. Two top-level
`{ provider, model }` pairs choose the models: `default_model` for
conversation sessions, and `small_model` for both session titles and automatic
tool-approval judgments. Each pair has the same form as a `[[moa]]` member: a
`[[providers]]` entry name plus a model id, written out.

```toml
# Top-level keys must precede table headers.
default_model = { provider = "chat", model = "claude-opus-4-6" }
small_model   = { provider = "helpers", model = "small-model-id" }

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
`OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL` override file URLs. No environment
variable selects a model.

## `default_model`

The model a new conversation session runs until it is explicitly switched. Its
provider is the session's default provider. There is no built-in model: when
the pair is absent, has no model id, or names no `[[providers]]` entry, startup
warns naming the key and sessions start in the deterministic fallback. No
provider is called, no model is reported for the session, and its first message
says that `default_model` selects no model. The model picker marks no entry as
the default in that case.

## `small_model`

The model titles and approval judgments run on. Its provider must be an
`openai-compatible` entry; that entry's `base_url` and `api_key_env` serve both
features. When the pair is absent, invalid, or names a non-openai-compatible
entry, startup warns naming the key, tabs keep their raw first-message titles,
and approvals stay with the human. The same happens when the entry's key
variable is unset. It never borrows credentials or an endpoint from another
entry.

## No provider entries

With no `[[providers]]` entries, a built-in OpenAI-compatible endpoint entry
named `default` exists (key variable `OPENAI_API_KEY`). It carries no model:
both pairs still have to name it, for example
`default_model = { provider = "default", model = "..." }`.

## Reload

Reload Config updates future title calls and new sessions. Each running session
retains its judge connection; its conversation model can be explicitly switched
using the latest accepted catalog. Title clients are pooled per accepted shell
configuration, judge clients per session. In-flight calls keep their captured
connection when configuration changes.

## Removed forms

The old `[provider]` table is rejected; startup keeps the warn-and-default
policy for invalid configuration, and reload refuses the file and keeps the
previously accepted settings. Rewrite it as a `[[providers]]` entry plus a
`default_model` pair.

The top-level `default_provider` and `auxiliary_provider` keys and the entry-level
`default_model` key are no longer read. Like any other unrecognized key they
are warned about as a probable typo and ignored. The environment variables
`HORIZON_RIG_MODEL`, `HORIZON_AGENT_JUDGE_MODEL`, and `HORIZON_AGENT_TITLE_MODEL`
are no longer read either.

Real user configuration is not modified by repository integration. Agent history
also needs the separate [history conversion](agent-history-format.md) before
the agent wire-v24 restart.
