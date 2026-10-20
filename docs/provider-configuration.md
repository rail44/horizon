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

## Context windows

Tier 1 history clearing (`docs/agent-compaction-design.md`) needs the model's
context window. An `openai-compatible` entry's `/models` listing is read first
(synthetic.new declares `context_length` and `max_output_length` there); a
listing that declares nothing -- standard OpenAI, Ollama Cloud (`id`,
`object`, `created`, `owned_by` only) -- defers to the model catalog,
models.dev's `api.json`, the catalog OpenCode lists models from. The entry is
matched to a catalog provider by its `base_url` (`https://ollama.com/v1` is
the catalog's `ollama-cloud`), and the model by exact id within it; the same
id under another provider is another model. The catalog is fetched once per
daemon start and cached at `$XDG_CACHE_HOME/horizon/models-dev-api.json`
(`~/.cache` when unset); a failed fetch uses the cached copy, and with
neither, or with a provider or model the catalog does not know, clearing
stays off for the session and agentd says so on stderr. No window is ever
guessed. `HORIZON_MODEL_CATALOG_URL` and `HORIZON_MODEL_CATALOG_CACHE`
relocate the fetch and the copy. Decided 2026-10-20 after the owner's long
session on Ollama ran to 620k tokens with clearing never firing; reading
Ollama's own `/api/show` was rejected as a provider-specific branch.

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
