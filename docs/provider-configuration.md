# Provider and model configuration

`[[providers]]` entries describe endpoints and credentials only, each
reached as a rig vendor (`vendor`). Three top-level `{ provider, model }`
pairs choose the models: `default_model` for
conversation sessions, `small_model` for session titles (and automatic
tool-approval judgments when no decision model is available), and
`decision_model` for automatic tool-approval judgments and turn-continuation
checks. Each pair is a `[[providers]]` entry name plus a model id, written
out.

```toml
# Top-level keys must precede table headers.
default_model = { provider = "chat", model = "claude-opus-4-6" }
small_model   = { provider = "helpers", model = "small-model-id" }
decision_model = { provider = "ollama", model = "clef:cloud" }

[[providers]]
name = "chat"
vendor = "anthropic"

[[providers]]
name = "helpers"
vendor = "openai"
api_key_env = "HELPER_API_KEY"
base_url = "https://your-openai-compatible-endpoint/v1"

[[providers]]
name = "ollama"
vendor = "ollama"
base_url = "https://ollama.com/v1"
decision_url = "https://ollama.com/v1/systemone"
```

Only environment-variable names go in TOML; credentials stay in the environment.
Endpoint URLs come only from the file: no environment variable overrides an
entry's `base_url` or `decision_url`. No environment variable selects a model.

## `vendor`

The rig vendor the entry is reached as, as rig-core's provider registry
spells it: `openai` (the default), `anthropic`, `ollama`, `openrouter`,
`deepseek`, `groq`, `mistral`, `moonshot`, `zai`, and the rest rig
registers. The vendor decides the dialect requests are spelled in -- which
generation options a request may carry and how each is written for that
provider -- and the vendor the model catalog files the entry's models under.
An entry with no `base_url` talks to the vendor's own endpoint (`ollama`'s
is a local daemon, so Ollama Cloud is `vendor = "ollama"` with
`base_url = "https://ollama.com/v1"`), and an entry with no `api_key_env`
reads the vendor's own variable (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
`OLLAMA_API_KEY`, ...).

An endpoint rig has no vendor for -- DeepInfra, synthetic.new -- is
`vendor = "openai"` with its own `base_url`. rig then spells its requests as
OpenAI's own and, for a model OpenAI's catalog does not list, checks nothing
about them; DeepInfra was measured to take OpenAI's `reasoning_effort` that
way (`docs/agent-reasoning-effort-design.md`). A vendor rig does not know, a
provider rig's catalog lists but rig cannot connect to, and a vendor on a
wire other than OpenAI's or Anthropic's are refused: agentd drops the entry
and says so on stderr.

## Context windows

Tier 1 history clearing (`docs/agent-compaction-design.md`) needs the model's
context window. An entry on OpenAI's wire has its `/models` listing read
first (synthetic.new declares `context_length` on each entry, DeepInfra under
the entry's `metadata`); a listing that declares nothing -- standard OpenAI,
Ollama Cloud (`id`, `object`, `created`, `owned_by` only) -- and an entry on
Anthropic's wire defer to the model catalog: rig-core's own
(`rig_core::catalog::Catalog::builtin`, models.dev with its main providers
reviewed by hand) with a newer copy of models.dev's `api.json` laid over it
by rig's rule, which keeps rig's hand-reviewed facts. The model is found by
the entry's vendor and its id (the id as listed, or a dated snapshot of a
listed id); the same id under another vendor is another model. A provider
rig has no vendor for has no models there: its listing is its only source.
The copy is fetched once per daemon start and cached at
`$XDG_CACHE_HOME/horizon/models-dev-api.json` (`~/.cache` when unset); a
failed fetch uses the cached copy when it is newer than rig's own data, and
with neither, rig's own catalog serves alone. A model neither source knows
leaves clearing off for the session. No window is ever guessed.
`HORIZON_MODEL_CATALOG_URL` and `HORIZON_MODEL_CATALOG_CACHE` relocate the
fetch and the copy. The same catalog is what a turn's requests are encoded
with.

## `default_model`

The model a new conversation session runs until it is explicitly switched. Its
provider is the session's default provider. There is no built-in model: when
the pair is absent, has no model id, or names no `[[providers]]` entry, startup
warns naming the key and sessions start in the deterministic fallback. No
provider is called, no model is reported for the session, and its first message
says that `default_model` selects no model. The model picker marks no entry as
the default in that case.

## `small_model`

The model titles run on, and automatic approval judgments when no decision
model is available (a two-stage chat judge over `/chat/completions`). Its
provider's vendor must speak OpenAI's wire (the judge reads the logprobs of
an OpenAI chat completion); that entry's `base_url` and `api_key_env` serve
it. When the pair is absent, invalid, or names an entry on another wire,
startup warns naming the key, tabs keep their
raw first-message titles, and that fallback judge is off. The same happens
when the entry's key variable is unset. It never borrows credentials or an endpoint
from another entry.

## `decision_model`

The decision model automatic approval judgments and turn-continuation checks
ask. A decision model generates no text: `POST {decision_url}` (the System
One API) takes a `state` plus named `choice`/`noul` questions and returns a
probability per option. Its provider must be an entry with a
`decision_url`: the route's full URL, used as written. It is not
derived from `base_url`, because providers serve it at unrelated paths —
Ollama cloud at `https://ollama.com/v1/systemone` (model `clef:cloud`),
DeepInfra at `https://api.deepinfra.com/v1/decisions` (model
`Cloudflare/clef`), outside its `/v1/openai` chat base — and no environment
variable overrides it. The entry's `api_key_env` serves it as a bearer token.
When the pair is absent, invalid, or names an entry without a
`decision_url`, startup warns naming
the key: automatic approval judgments fall back to `small_model`'s chat
judge (and stay with the human when that pair is unavailable too), and
turn-continuation checks are off. The same happens when the entry's key
variable is unset. Ollama cloud serves decision models only on a paid plan or
with credits.

## No provider entries

With no `[[providers]]` entries, a built-in entry named `default` exists on
OpenAI's vendor (key variable `OPENAI_API_KEY`). It carries no model:
every pair still has to name it, for example
`default_model = { provider = "default", model = "..." }`.

## Reload

Reload Config updates future title calls and new sessions. Each running session
retains its decision connection (which the judge runs on); its conversation model can be explicitly switched
using the latest accepted catalog. Title clients are pooled per accepted shell
configuration, decision clients per session. In-flight calls keep their captured
connection when configuration changes.

## Removed forms

The old `[provider]` table is rejected; startup keeps the warn-and-default
policy for invalid configuration, and reload refuses the file and keeps the
previously accepted settings. Rewrite it as a `[[providers]]` entry plus a
`default_model` pair.

An entry's `kind` (`"openai-compatible"` / `"anthropic"`) was replaced by
`vendor`. A file whose entry still writes it is refused whole,
as the `[provider]` table is, rather than read as OpenAI's vendor (which
would send an Anthropic key to OpenAI): `kind = "anthropic"` becomes
`vendor = "anthropic"`; `kind = "openai-compatible"` (or no `kind`) becomes
the vendor rig has for that endpoint when it has one (`ollama` for Ollama,
`openrouter` for OpenRouter, ...), else `vendor = "openai"` with the entry's
`base_url`.

The top-level `default_provider` and `auxiliary_provider` keys and the entry-level
`default_model` key are no longer read. Like any other unrecognized key they
are warned about as a probable typo and ignored. The environment variables
`HORIZON_RIG_MODEL`, `HORIZON_AGENT_JUDGE_MODEL`, and `HORIZON_AGENT_TITLE_MODEL`
are no longer read either.

Real user configuration is not modified by repository integration. Agent history
also needs the separate [history conversion](agent-history-format.md) before
the agent wire-v24 restart.
