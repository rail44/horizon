# Agent Web Search APIs — de facto standards and provider survey (2026-07-19)

Research pass for backlog 18 (give Horizon's agent sessions a web-search
tool). All facts below carry a source URL; pricing/rate-limit numbers are
current as of **2026-07-19** and are the fastest-moving part of this
document — re-verify before acting on a specific number. Complementary to
`docs/research/crush-opencode-tools-2026-07-07.md` (source-level read of
crush/opencode's actual tool code, recovered 2026-07-19) — that doc covers
implementation and trust-boundary mechanics in depth; this one focuses on
the schema-standardization question and a wider provider survey, and
corrects two of its own web-search-derived claims against that doc's
first-hand source reading (noted inline in section 4).

## Context

Horizon is a desktop agent workspace that wants to give its agent sessions
a web-search tool (backlog 18: "give the agent outward web search — the
'search tool' the owner originally meant"). Horizon already treats the
OpenAI-compatible Chat/Responses surface as the de facto standard for LLM
providers, deliberately avoiding vendor lock-in (`AGENTS.md`
"Configuration"; `crates/horizon-agent`). The owner's stated priority for
search is the same axis: **does an equivalent de facto standard exist for
agent-facing web search**, so Horizon can ride it instead of hand-rolling a
vendor-specific integration? This report answers that question first
(section 1), then surveys what "agent-friendly" actually means in the
responses providers return (section 2), compares the leading providers on
practical integration criteria (section 3), checks what shipped coding
agents actually do today (section 4), and closes with implications for
Horizon (section 5) — no decision is made here.

## 1. Is there a de facto standard?

**Short answer: no schema-level standard exists. There is a transport-level
standard (MCP), but it does not standardize what a "web search" tool's
input/output looks like — every vendor's MCP server defines its own tool
name and shape.**

### MCP: standardizes plumbing, not the search contract

MCP (Model Context Protocol) is genuinely a de facto standard for *exposing
tools to an LLM* — as of March 2026 it has 97M+ monthly SDK downloads, 81k+
GitHub stars, and native support from every major vendor (Anthropic, OpenAI,
Google, Microsoft, AWS), and Anthropic donated it to the Linux
Foundation-hosted Agentic AI Foundation in December 2025, making it
vendor-neutral governance rather than an Anthropic asset
([essamamdani.com MCP guide](https://essamamdani.com/blog/complete-guide-model-context-protocol-mcp-2026)).
Every major search vendor now ships an **official** MCP server:

- Exa — `exa-labs/exa-mcp-server`, hosted at `mcp.exa.ai/mcp`, no API key required for the hosted tier ([github.com/exa-labs/exa-mcp-server](https://github.com/exa-labs/exa-mcp-server), [exa.ai/mcp](https://exa.ai/mcp))
- Tavily — `tavily-ai/tavily-mcp`, hosted at `mcp.tavily.com/mcp/?tavilyApiKey=...` ([github.com/tavily-ai/tavily-mcp](https://github.com/tavily-ai/tavily-mcp), [docs.tavily.com/documentation/mcp](https://docs.tavily.com/documentation/mcp))
- Brave — `brave/brave-search-mcp-server`, now the canonical replacement for Anthropic's original archived reference implementation (see below) ([github.com/brave/brave-search-mcp-server](https://github.com/brave/brave-search-mcp-server))
- Parallel — official Search/Task MCP at `task-mcp.parallel.ai/mcp` ([pulsemcp.com/servers/parallel-search](https://www.pulsemcp.com/servers/parallel-search)); opencode's own source also targets `search.parallel.ai/mcp` directly (see section 4)
- Perplexity — `perplexityai/modelcontextprotocol`, exposing Sonar models and the Search API ([github.com/perplexityai/modelcontextprotocol](https://github.com/perplexityai/modelcontextprotocol))
- Jina — `jina-ai/MCP`, hosted at `mcp.jina.ai`, wraps Reader/search/embeddings/reranker ([github.com/jina-ai/MCP](https://github.com/jina-ai/MCP))
- Firecrawl — `firecrawl/firecrawl-mcp-server` ([github.com/firecrawl/firecrawl-mcp-server](https://github.com/firecrawl/firecrawl-mcp-server))
- Kagi — `kagisearch/kagimcp`, exposing `kagi_search_fetch` / `kagi_extract` ([github.com/kagisearch/kagimcp](https://github.com/kagisearch/kagimcp))

Notably, **Brave Search was one of Anthropic's original MCP reference
servers** at launch (alongside filesystem, git, GitHub, Slack, etc.) — it
was later archived from `modelcontextprotocol/servers` in favor of Brave's
own officially-maintained server, "to reduce maintenance overhead"
([github.com/modelcontextprotocol/servers-archived](https://github.com/modelcontextprotocol/servers-archived)).
That history matters: even MCP's own maintainers didn't try to keep a
canonical "search" reference schema going — they handed it to each vendor.
The result is exactly what you'd expect: `web_search_exa`,
`tavily-search`/`tavily-extract`, `brave_web_search`, `kagi_search_fetch`,
each with its own parameter names (`numResults` vs `max_results` vs
`limit`), its own filters, and its own result shape. **MCP standardizes the
JSON-RPC transport and tool-discovery handshake, not the search tool's
argument/return contract.** Swapping the underlying MCP server still means
the agent (or your prompt/tool-normalization layer) sees a different tool
name and schema. (opencode's own source confirms this cost is small enough
to absorb without a generic MCP client at all — see section 4: it talks to
Exa's and Parallel's MCP endpoints with a bespoke lightweight JSON-RPC
client, not a full MCP SDK, because "the transport is simple enough, only
the two tool schemas differ.")

### Hosted "web_search" tools: two incompatible schemas, not one

Both OpenAI and Anthropic ship a first-party, server-executed web search
tool baked into their own completion APIs — but the two are unrelated
designs, not implementations of one shared spec:

**OpenAI Responses API** ([developers.openai.com/api/docs/guides/tools-web-search](https://developers.openai.com/api/docs/guides/tools-web-search)):

```json
// request
{ "model": "gpt-5.6", "tools": [{ "type": "web_search" }] }
```
Response is a `web_search_call` output item (`action.query`, `action.sources`)
followed by a `message` item whose `output_text` content carries
`annotations: [{ "type": "url_citation", "start_index", "end_index", "url", "title" }]`.
Supports `filters.allowed_domains`/`blocked_domains`, `user_location`,
`search_context_size` (low/medium/high), and `external_web_access: false`
for cache-only mode.

**Anthropic Claude API** ([platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool](https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool)):

```json
// request
{ "type": "web_search_20250305", "name": "web_search", "max_uses": 5,
  "allowed_domains": ["example.com"] }
```
Response is a `server_tool_use` block (the query) followed by a
`web_search_tool_result` block containing `web_search_result` items
(`url`, `title`, `page_age`, `encrypted_content` — must be echoed back
verbatim on the next turn). The model's own text then carries
`citations: [{ "type": "web_search_result_location", "url", "title",
"encrypted_index", "cited_text" }]`. Claude uses **Brave Search** as its
backend index ([tryprofound.com](https://www.tryprofound.com/blog/what-is-claude-web-search-explained),
subprocessor disclosure); the newer `web_search_20260209`/`_20260318`
versions run the search *inside code execution* so the model can filter
results in a sandbox before they hit its context window ("dynamic
filtering") — a token-efficiency idea neither OpenAI's design nor any
third-party API currently has an equivalent for.

Field names, block types, and the citation-encoding scheme (opaque
`encrypted_content`/`encrypted_index` vs plain `start_index`/`end_index`)
share no lineage. A client built against one cannot point at the other by
changing a base URL — unlike chat completions, where the two vendors'
request/response shapes are close enough that "OpenAI-compatible" is a
meaningful category. **Web search is not in that category.**

### OpenRouter: the closest thing to a cross-vendor normalization, but it's one aggregator's convention, not a standard

OpenRouter — the kind of OpenAI-compatible aggregator Horizon's provider
abstraction already resembles — layers its own `web` plugin over whatever
model you call, and it has to solve exactly the "which backend" problem a
Horizon design would face:

```json
{ "model": "openai/gpt-5.2",
  "plugins": [{ "id": "web", "engine": "exa", "max_results": 5 }] }
```
For providers with native search (OpenAI, Anthropic, Google, Perplexity,
xAI) it passes through to their own tool; for everyone else it falls back
to **Exa** by default, and normalizes the result into an
OpenAI-Chat-Completions-shaped `annotations: [{ "type": "url_citation",
"url_citation": { "url", "title", "content", "start_index", "end_index" } }]`
([openrouter.ai/docs/guides/features/plugins/web-search](https://openrouter.ai/docs/guides/features/plugins/web-search)).
Pricing: Exa engine $0.005/request (10 results, +$0.001/extra), Parallel
$0.001/request, Perplexity $0.005/request, native passed through at the
underlying provider's price. This confirms the pattern rather than
refuting the "no standard" conclusion: OpenRouter had to *pick a default
vendor and write a normalization layer* — it didn't find a standard to
adopt either.

**synthetic.new / other OpenAI-compatible inference resellers**: found no
evidence of a distinct, documented web-search endpoint or tool on
synthetic.new specifically — its docs describe OpenAI/Anthropic-compatible
chat completion routing only ([dev.synthetic.new/docs/api/overview](https://dev.synthetic.new/docs/api/overview)).
**Low confidence** on this being a complete negative (could not find a
dedicated search-feature page); treat as "not found," not "confirmed
absent."

### Old and adjacent standards — not applicable

**OpenSearch (2005, A9/Amazon)** is a real, ratified, still-supported
spec — an XML description document plus a `<link rel="search"
type="application/opensearchdescription+xml">` autodiscovery convention for
*browsers* to add a site's search box to their search-engine list
([developer.mozilla.org/.../OpenSearch](https://developer.mozilla.org/en-US/docs/Web/XML/Guides/OpenSearch),
[Wikipedia](https://en.wikipedia.org/wiki/OpenSearch_(specification))). It
solves a different problem (site search discovery for human browser UX,
templated GET URLs) with no concept of LLM-consumable content extraction,
citations, or JSON. It predates and is orthogonal to agent web search; no
current agent product references it. **Not a candidate.**

**WebMCP** (Google/Microsoft-backed, shipping as a Chrome 146 canary
preview) is a genuinely emerging standard, but it standardizes how a
*web page itself* exposes callable tools to an in-browser agent — the
inverse direction from "agent calls an external search API"
([amdatalakehouse.substack.com state-of-agentic-AI-standards](https://amdatalakehouse.substack.com/p/the-state-of-agentic-ai-standards)).
Not applicable to Horizon's terminal/agent-pane shell, which has no
browser-embedding surface.

### Verdict

No vendor-neutral schema standard exists for "the web search tool" the way
OpenAI-compatible chat completions is a de facto standard for "the LLM
API." MCP standardizes the wire protocol every vendor now ships an official
server over, which is valuable (one client library, one auth/transport
pattern) — but the tool's own contract (name, params, result shape) is
still bespoke per vendor and has to be normalized by whoever consumes it,
MCP or not.

## 2. What "agent-friendly" looks like in practice

Despite no shared schema, there's real convergence on *what agents want*,
visible across every provider surveyed:

- **Clean extracted text/markdown, not a raw SERP.** Every dedicated
  agent-search API (Exa, Tavily, Jina, Firecrawl, Kagi) returns page
  content already stripped of nav/ads/boilerplate — not just a link + a
  two-line snippet the way a classic search-engine results page would.
  Jina explicitly brands this "LLM-ready markdown"; Firecrawl's tagline is
  "clean, agent-ready context."
- **Search + full-content in one call.** Tavily's `include_raw_content`
  and Exa's `contents: { text, highlights, summary }` let a single request
  return both the ranked result list *and* full/cleaned page bodies,
  avoiding a second fetch round-trip per result. Anthropic's and OpenAI's
  hosted tools are search-only (the model gets snippets/titles, not full
  page text) — full-page follow-up is a separate `web_fetch` tool call
  ([Anthropic web fetch tool](https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-fetch-tool)).
- **Citations as first-class, structured objects**, but three different
  shapes:
  - OpenAI: `annotations[].url_citation` with `start_index`/`end_index`
    character offsets into the plain output text.
  - Anthropic: `citations[].web_search_result_location` with an opaque
    `encrypted_index` (must round-trip) plus up to 150 chars of
    `cited_text`; citation fields (`cited_text`/`title`/`url`) are
    explicitly **not** billed as tokens.
  - Exa/Tavily: no citation object at all — the caller gets a flat
    `results[]` array (url/title/text/score) and is expected to build its
    own attribution if it's synthesizing an answer itself.
- **Token-efficiency mechanisms are provider-specific.** Anthropic's
  `web_search_20260209`+ dynamic filtering runs the search *inside a code
  execution sandbox* so irrelevant results never reach the model's context
  at all. Exa's `contents.text.maxCharacters`/`verbosity` and Tavily's
  `chunks_per_source` let the caller cap how much raw text comes back per
  result. There's no shared mechanism, but the direction (let the caller
  bound context cost per result) is universal.

### Concrete request/response examples (4 providers)

**Anthropic** (`web_search_20250305`) — request:
```json
{"tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 5}]}
```
response (abridged):
```json
{"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01WYG3...",
 "content": [{"type": "web_search_result", "url": "https://en.wikipedia.org/wiki/Claude_Shannon",
              "title": "Claude Shannon - Wikipedia",
              "encrypted_content": "EqgfCioIARgBIiQ3YTAwMjY1Mi1m...",
              "page_age": "April 30, 2025"}]}
```

**OpenAI Responses API** — request:
```json
{"model": "gpt-5.6", "tools": [{"type": "web_search"}]}
```
response item:
```json
{"type": "web_search_call", "id": "ws_...", "status": "completed",
 "action": {"type": "search", "query": "...", "sources": [{"url": "...", "title": "..."}]}}
```
plus a `message` item with `annotations: [{"type": "url_citation", "start_index": 100, "end_index": 250, "url": "...", "title": "..."}]`.

**Tavily** `/search` — request:
```json
{"query": "...", "search_depth": "advanced", "include_raw_content": "markdown", "max_results": 5}
```
response (abridged):
```json
{"query": "...", "answer": "...", "results": [
  {"title": "...", "url": "...", "content": "...", "score": 0.83, "raw_content": "..."}
], "response_time": 1.2}
```

**Exa** `/search` — request:
```json
{"query": "...", "numResults": 10,
 "contents": {"text": true, "highlights": {"maxCharacters": 2000}}}
```
response (abridged):
```json
{"requestId": "...", "results": [
  {"title": "...", "url": "...", "publishedDate": "...", "author": "...",
   "text": "full page content...", "highlights": ["Key snippet 1"], "highlightScores": [0.46]}
], "costDollars": {"total": 0.007, "search": {"neural": 0.007}}}
```

## 3. Provider comparison (2026-07-19 snapshot)

| Provider | API shape | Auth | Pricing (public tier) | Free tier | Content extraction | Official MCP |
|---|---|---|---|---|---|---|
| **Exa** | REST, `contents` sub-object for extraction | API key | $7/1k search requests (10 results); Contents $1/1k pages ([exa.ai/pricing](https://exa.ai/pricing)) | $10 new-account credit + $7/mo recurring (with card on file); 1,000 free searches/mo | Full text, highlights, summary, subpages — single call | Yes, hosted, no key needed for hosted tier ([exa.ai/mcp](https://exa.ai/mcp)) |
| **Tavily** | REST, credit-based | API key | $0.008/credit; basic search = 1 credit, advanced = 2 ([docs.tavily.com/documentation/api-credits](https://docs.tavily.com/documentation/api-credits)) | 1,000 free credits/mo, no card | `include_raw_content` (markdown/text) in same call; also map/crawl endpoints | Yes, hosted (`mcp.tavily.com`) ([docs.tavily.com/documentation/mcp](https://docs.tavily.com/documentation/mcp)) |
| **Brave Search** | REST | API key (subscription token) | $5/1,000 requests, 50 req/s ([api-dashboard.search.brave.com/documentation/pricing](https://api-dashboard.search.brave.com/documentation/pricing)) | $5/mo free credits (~1,000 queries) | Snippets only (no full-text extraction endpoint in Search plan) | Yes, official (`brave/brave-search-mcp-server`) |
| **Parallel** | REST, tiered "processors" | API key | Turbo $1/1k (~200ms); Basic/Advanced $5/1k (1s/3s) ([parallel.ai/pricing](https://parallel.ai/pricing)) | Not found in this pass | Excerpts, not stated as full-page by default | Yes, official (`task-mcp.parallel.ai/mcp`) |
| **Perplexity Sonar** | Chat-completion-style + Search API | API key | Token pricing $0.20–$15/1M by tier + per-request fee $5–$22/1k depending on model/context ([docs.perplexity.ai/docs/getting-started/pricing](https://docs.perplexity.ai/docs/getting-started/pricing)) | None found; tiered rate limits by lifetime credit purchase (20–100 RPM) | Answer is model-synthesized text with citations, not raw page content | Yes, official (`perplexityai/modelcontextprotocol`) |
| **Jina (Reader/`s.jina.ai`)** | Simple GET (`https://s.jina.ai/?q=`) | Optional API key (works keyless, rate-limited) | Token-based; ~$20/mo for extra tokens+QPS | 10M free tokens per new key; keyless tier exists | Clean markdown per result, explicitly LLM-oriented | Yes, hosted (`mcp.jina.ai`) |
| **Firecrawl** | REST, credit-based | API key | Hobby $16/mo (3,000 credits); search = 2 credits/10 results ([firecrawl.dev/pricing](https://www.firecrawl.dev/pricing)) | Free tier: 5 searches/min, 10 scrapes/min | JS-rendered, anti-bot-hardened scrape to clean markdown | Yes, official (`firecrawl/firecrawl-mcp-server`) |
| **Kagi** | REST | API key | Search $12/1k requests; Extract $4/1k pages ([kagi.com/api/pricing](https://kagi.com/api/pricing)) | None found (invoiced at $100 usage or 30 days) | Extract endpoint returns clean markdown, separate call from search | Yes, official (`kagisearch/kagimcp`) |
| **SerpAPI** | REST, structured SERP scrape | API key | $25/mo = 1,000 searches, up to $275/mo = 30,000 ([serpapi.com/pricing](https://serpapi.com/pricing)) | 250 searches/mo, 50/hour throughput | Structured SERP fields (organic results, snippets) — not page-body extraction | Not found (competitor SearchApi.io has one) |
| **DuckDuckGo Lite** | None — undocumented HTML scrape | None | Free | Unlimited but fragile (no ToS-sanctioned API) | Title/URL/snippet only, no extraction | No |

Notable outlier: **SerpAPI is a SERP-scraper**, structurally different from
the rest — it returns Google's/Bing's results page as structured JSON
(ads, knowledge panels, organic snippets), not agent-oriented clean content.
It's the "old paradigm" (screen-scrape a search engine) that Exa/Tavily/
Jina/Firecrawl were built to replace for LLM consumption specifically.
DuckDuckGo Lite (no official row above — it's not a product, just an
undocumented endpoint) is the same old paradigm at zero cost and zero
reliability guarantee; see crush in section 4 for a real shipped agent
depending on it.

## 4. What existing agent products actually use

- **Claude Code**: uses Anthropic's own hosted `web_search` tool
  server-side, backed by Brave Search's index
  ([tryprofound.com/blog/what-is-claude-web-search-explained](https://www.tryprofound.com/blog/what-is-claude-web-search-explained)).
  Not a separate vendor integration Claude Code itself built — it inherits
  whatever the Claude API ships.
- **OpenAI Codex CLI**: web search is on by default via OpenAI's own
  maintained "web search cache" (an OpenAI-hosted index), configurable in
  `config.toml` as `web_search = "cached" | "indexed" | "live" | "disabled"`
  — again riding the Responses API's built-in `web_search` tool rather than
  calling a third party
  ([developers.openai.com/codex/config-reference](https://developers.openai.com/codex/config-reference),
  [codesignal.com Enabling Web Search lesson](https://codesignal.com/learn/courses/codex-configuration-extensibility/lessons/enabling-web-search)).
- **Gemini CLI**: ships its own `google_web_search` tool
  (`packages/core` `WebSearchTool`), which calls the Gemini API's Google
  Search grounding and gets back a synthesized summary plus
  source URIs/titles for citation — again a first-party hosted capability,
  not a third-party API call
  ([github.com/google-gemini/gemini-cli/blob/main/docs/tools/web-search.md](https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/web-search.md)).
- **opencode** (sst/anomalyco): **does ship a built-in `websearch` tool**
  (`tool/websearch.ts`) — this corrects a wrong first-pass finding of mine
  from secondary web-search sources (community plugins, an open
  first-party-tool-passthrough issue) that read as "no built-in search."
  The authoritative source is `docs/research/crush-opencode-tools-2026-07-07.md`
  in this repo, a prior project-session pass that read opencode's actual
  source at `d341d84b24fb`/`dev` HEAD (2026-07-06/07): the tool talks to
  either **Exa** (`mcp.exa.ai/mcp`, tool name `web_search_exa`) or
  **Parallel** (`search.parallel.ai/mcp`, tool name `web_search`) — chosen
  ~50/50 by a checksum of the session ID, overridable via
  `OPENCODE_WEBSEARCH_PROVIDER`/`OPENCODE_ENABLE_EXA`/`OPENCODE_ENABLE_PARALLEL`
  — over a **bespoke lightweight JSON-RPC client the opencode team wrote
  themselves, not a full MCP SDK** (`tool/mcp-websearch.ts`). It returns
  the upstream MCP response's `content[0].text` almost verbatim, delegating
  all formatting to the provider. Auto-enabled only when opencode's own
  hosted gateway is in use; with a self-supplied API key it's disabled by
  default unless explicitly flagged on. (The community plugins found in my
  first pass — `opencode-websearch`, `opencode-websearch_duckduckgo`,
  `opencode-websearch-cited` — are additional/alternative options
  layered on top of, not filling a gap left by, this built-in tool.)
- **Crush** (charmbracelet): **does ship a built-in `web_search` tool** —
  another correction of mine from secondary sources (I'd read an open
  issue asking Crush to add OpenAI's *native hosted* `web_search` tool and
  wrongly concluded Crush had no search tool at all). Per the same prior
  source-reading pass: Crush's `web_search` (`web_search.go`, `search.go`)
  scrapes `lite.duckduckgo.com/lite/` HTML directly — no official API, no
  key, DOM-walked with `golang.org/x/net/html`, with 11 rotating
  User-Agents and 500–2000ms jittered delays to dodge blocking. It's not
  exposed to the top-level coding agent directly; it only exists inside a
  throwaway `agentic_fetch` subagent, gated by one outer
  `permissions.Request(Action: "fetch")` approval, after which
  `web_search`/`web_fetch`/`glob`/`grep`/`view` run unapproved inside that
  subagent via `AutoApproveSession` — a "one outer approval, inner
  search/fetch chain" shape backlog 18 explicitly flags as close to
  Horizon's own delegation/skill mechanism. The still-open issue
  (`charmbracelet/crush#2777`) is about adding OpenAI's *native* hosted
  tool as an **additional** option alongside the existing DDG scrape, not
  about filling a complete gap — a maintainer noted Charm's other product
  "Fantasy already supports `web_search`," implying the capability exists
  elsewhere in Charm's stack but isn't wired into Crush yet
  ([github.com/charmbracelet/crush/issues/2777](https://github.com/charmbracelet/crush/issues/2777)).
- **Aider**: has no search tool at all, built-in or otherwise — only a
  `/web <url>` command that scrapes one given URL via Playwright (falling
  back gracefully when Playwright is unavailable) and drops the markdown
  into chat context. This is fetch, not search
  ([github.com/Aider-AI/aider/blob/main/aider/scrape.py](https://github.com/Aider-AI/aider/blob/main/aider/scrape.py)).
- **Cline**: ships web search as an optional plugin
  (`cline/plugins/plugins/web-search`) that calls **Exa** and returns
  normalized result metadata for discovery, with page content then fetched
  through Cline's normal browsing tools — plus general MCP support so users
  can wire in any other vendor's MCP server themselves
  ([github.com/cline/plugins/tree/main/plugins/web-search](https://github.com/cline/plugins/tree/main/plugins/web-search)).

**Pattern across all seven**: products built by the same company as an LLM
(Claude Code/Anthropic, Codex CLI/OpenAI, Gemini CLI/Google) default to
that vendor's own hosted search tool and never touch a third-party search
API. Independent/multi-model products that do ship a built-in tool
(opencode, Crush, Cline) all pick **one specific backend** (opencode:
Exa or Parallel behind a self-written thin JSON-RPC client; Crush: DDG
scraping; Cline: Exa) and wrap it behind their **own** tool name/schema —
never the vendor's raw MCP or REST shape passed straight through to the
model. opencode and Crush both additionally treat provider choice as a
config knob (env vars; an open per-provider-option issue), not a hardcoded
one-vendor-forever decision. Aider is the one outlier with no search
capability whatsoever.

## 5. Recommendations for Horizon (not a decision — options for the owner)

Given section 1's conclusion (no schema standard exists) and section 4's
observed pattern (every multi-vendor product normalizes behind its own thin
tool, even when it happens to pick one vendor), three shapes are worth
weighing. They are not mutually exclusive (C can be an implementation
strategy under B).

**A — Pass through the LLM provider's own hosted tool.** When Horizon's
configured `[provider]` is OpenAI (or an OpenAI-compatible aggregator that
proxies a `web_search`-shaped tool, e.g. OpenRouter's `web` plugin), forward
`tools: [{"type": "web_search"}]` as-is. Zero new infrastructure, matches
what Claude Code/Codex CLI/Gemini CLI already do for their own vendor.
Downside: it stops working the moment the configured `base_url` points at
a self-hosted or third-party backend without that extension (a plain vLLM
server, most OpenAI-compatible resellers) — which directly conflicts with
the provider-agnostic stance `[provider].base_url` exists to protect, and
it means Anthropic-configured sessions get a completely different (and
currently mutually incompatible) tool contract than OpenAI-configured ones.

**B — A thin internal search trait with a swappable adapter**, structurally
parallel to how `horizon-agent` already owns the bash/fs tool contracts
rather than exposing a vendor's raw shape. The model always sees one
Horizon-owned tool schema (e.g. `web_search(query) -> results: [{url,
title, content, published_date}]`); a backend adapter (Tavily, Exa, Brave,
...) is selected via config and does the shape translation. This is what
"agent-friendly format" cashed out to in section 2's survey regardless of
vendor — clean extracted content, not raw SERP — so the normalized shape
is cheap to define well. It's also exactly what opencode and Crush both
independently converged on (section 4): one product-owned tool name,
provider swappable via config, never the vendor's raw schema exposed to
the model. Vendor swap = write one new adapter; no protocol lock-in
survives past that one module. Cost: Horizon owns and maintains the
adapter code, and picks (and pays for, or scrapes for free at DuckDuckGo's
reliability cost) a default vendor — mirroring exactly the choice
OpenRouter made when it defaulted its own "web" plugin to Exa.

**C — Delegate to MCP and let the owner point at any vendor's official
server.** `rig-core` (already used for the agent loop) exposes `rmcp`
support per the loop-engineering research note, so wiring an MCP client
into `horizon-agent` is comparatively cheap, and every vendor surveyed now
ships an official server (section 1) — opencode's own precedent
(section 4) shows a project can get away with an even smaller bespoke
JSON-RPC client instead of a generic MCP SDK, since the transport really is
simple. This buys close-to-zero-code vendor swapping at the transport
layer, but section 1's core finding still applies: MCP doesn't normalize
the tool's own schema, so swapping from Exa's MCP server to Tavily's
changes the tool name and arguments the model has been prompted/shown
examples for — meaning a thin normalization shim in front of whichever MCP
server is selected is needed for the swap to actually be invisible to the
agent, which is B's shim wearing MCP (or a bespoke JSON-RPC client, per
opencode) as its transport instead of direct HTTP.

Orthogonal to all three: backlog 18 also flags a trust-boundary question —
whether search/fetch should be gated behind a throwaway subagent with one
outer approval (Crush's `agentic_fetch` shape, close to Horizon's own
delegation/skill mechanism) rather than exposed as a direct top-level tool.
That design axis is out of this report's scope (this pass focused on the
standardization/schema question); see
`docs/research/crush-opencode-tools-2026-07-07.md` section "権限・安全" for
the crush/opencode approval mechanics in detail if picking that up next.

## Key findings

1. **標準は無い、と明確に言える。** MCPは「ツールを晒す配線層」としては本物のデファクト標準(Linux Foundation傘下、全大手ベンダーが公式実装)だが、「web検索ツールの引数・返り値の形」は標準化していない — Exa/Tavily/Brave/KagiのMCPサーバはそれぞれ別のツール名・別のスキーマを持つ。AnthropicとBraveがMCP最初期のreference実装をBrave専用に手放した経緯自体が「共通スキーマの維持は諦めた」証拠。
2. **OpenAIとAnthropicのホスト型web_searchツールも互いに非互換。** ブロック型・引用の符号化方式(平文index vs 暗号化index)まで別設計 — 「OpenAI互換」がLLM APIで意味を持つのとは対照的に、web検索では意味を持たない。
3. **「エージェントに使いやすい形式」は業界収斂している** — SERPの生リンクではなく、抽出済みクリーンテキスト/markdown、検索+本文取得が1コール、構造化された引用。Exa/Tavily/Jina/Firecrawlは軒並みこの形。SerpAPIとDuckDuckGo Liteだけは旧来のSERPスクレイプで毛色が違う。
4. **既存エージェント製品の実装パターンが一番示唆的**: 自社LLMを持つ製品(Claude Code/Codex CLI/Gemini CLI)は自社ホスト型ツールにそのまま乗る。マルチベンダー製品(opencode/Crush/Cline)は実装済みで、いずれも「モデルに見せるツールスキーマは自前で持ち、裏側のベンダーだけ設定で差し替え可能にする」を独立に採用— opencodeはExa/Parallelを自前の軽量JSON-RPCクライアントで、CrushはDuckDuckGo Liteスクレイプを`agentic_fetch`という使い捨てサブエージェント経由でゲート。**この2点は前回の(2次情報ベースの)自分の調査結果を訂正する** — 一次情報(`docs/research/crush-opencode-tools-2026-07-07.md`のソースコード読み取り)の方が正しい。
5. **Horizonへの含意**: (A) OpenAI互換プロバイダのホスト型ツールにそのまま乗る、(B) 自前の薄いトレイト+アダプタでベンダー交換可能にする、(C) MCPクライアント(または opencode 式の自前軽量クライアント)に委譲しベンダー公式サーバを使う、の3案。BとCは排反ではない。opencode/Crushの実例はどちらも実質B(モデルには自前スキーマ、裏だけ差し替え)の実装で、Cは輸送層をMCPにするかの選択に過ぎない。Aは`[provider].base_url`の汎用性と衝突する。
6. **確信度が低い箇所**: synthetic.new固有のweb検索APIの有無(見つからなかった=無いと断定はしていない)、Parallel/Perplexityの無料枠の有無(このパスでは見つからず)、Kagiのサマライザ(FastGPT)API価格(公式ページに記載なし)。
7. **本レポートのスコープ外**: 承認・信頼境界の設計(crushの`agentic_fetch`型ゲート)はbacklog 18のもう一つの論点だが、今回は標準化/スキーマの問いに絞った。次に着手するなら `docs/research/crush-opencode-tools-2026-07-07.md` の権限節が詳しい。

## Sources

Primary sources are inlined above at each claim. No further consolidated
list — every non-obvious number or claim in this document carries its own
URL at the point of use.

## Addendum 2026-07-20: quality/latency evidence and the vendor decision (Exa)

The owner pushed back on deciding from price/free-tier alone (those are
commoditized) and asked for quality, response-usability, and speed
evidence. Two passes were run on 2026-07-20; raw probe data (24 response
JSONs + timings) was captured in-session (ephemeral, not committed).

### Empirical probe: Parallel vs Exa free hosted MCP endpoints

Both vendors run keyless hosted MCP endpoints — the exact pair opencode
uses in production (`search.parallel.ai/mcp`, Parallel basic-mode
equivalent; `mcp.exa.ai/mcp`). Six dev-representative queries with
verifiable ground truth (Landlock ABI v6 kernel version, portable-pty
`close_random_fds`, cargo build-dir staleness across worktrees, tokio
`select!` biased semantics, GPUI custom elements, synthetic.new
pricing), each run twice per endpoint (cold/warm), 24 search calls
total, zero rate-limit or protocol errors.

| Measure | Parallel | Exa |
|---|---|---|
| Quality (reached=2 / near=1 / missed=0, max 12) | 9 | **12** |
| Latency, cold avg | 1.88s | 1.23s |
| Latency, warm avg | 1.84s (no caching observed) | **0.32s** (same-query cache) |
| Reproducibility | result count varied (7 vs 10) and top URLs differed across identical runs | top-3 identical across runs, all six queries |
| Response format | `content` is a stringified-JSON dump (markdown escapes leak through) duplicated by `structuredContent` (~2x wire bytes); **no result-count argument at all** (~7k tokens/call forced) | plain `Title/URL/Published/Highlights` text blocks, LLM-readable as-is; `numResults` + `maxCharacters` give the caller token-budget control |

Parallel's three dropped queries were characteristic: the Landlock
excerpts never contained "6.12"; the cargo-worktree query returned
different results on identical reruns; the synthetic.new query's top
excerpt was an unrendered "Loading provider…" JS placeholder. Exa hit
the exact docs.rs function page for portable-pty, reached
rust-lang/cargo#16642 (the same defect family as this repo's backlog
43), and returned all-official-domain results for synthetic.new.
Fairness note: Parallel's paid REST `turbo` mode (self-reported 216ms,
$1/1k) was not probed; it is a lighter mode than the probed basic, so a
quality reversal is implausible.

### Quality/latency literature

- The one systematic independent benchmark found (AIMultiple "Agentic
  Search in 2026", 2026-05-25; 100 queries x 8 APIs, GPT-5.2 judge, 10%
  human-reviewed): **Brave #1 overall (14.89, 669ms)**, Firecrawl 14.58,
  **Exa 14.39 (~1.2s; strongest in the technical-documentation
  category)**, Parallel Pro 14.21 (13.6s), Tavily 13.67, Parallel Base
  13.50 (2.9s), Perplexity 12.96 (11s+), SerpAPI 12.28. Jina untested.
  (https://aimultiple.com/agentic-search)
- Perplexity shows the largest claim-vs-independent gap (self-reported
  median 358ms vs 11s+ measured) — its published numbers cannot be
  trusted at face value.
- Parallel has **no independently-verified strength**: every favorable
  number is self-published (and drifts across snapshots, BrowseComp
  27%→51%); its CEO confirmed on HN that latency is deliberately traded
  for multi-hop accuracy — a research-agent orientation, not the
  short-factual-lookup profile of a coding agent.
- Tavily: independent user complaint of flaky JS-rendered-page handling.
  Jina: essentially no independent data. Firecrawl: high independent
  relevance scores but unclear whether search *discovery* is its own or
  upstream-dependent.

### Decision (owner, 2026-07-20)

**Search vendor = Exa.** Grounds: our probe (12/12, fastest measured,
deterministic, LLM-ready format with token-budget control) and the
independent benchmark's technical-documentation result agree; Parallel's
counter-case rests entirely on unverified self-published numbers, and
its stated design center (multi-hop research) is not Horizon's use case.
DuckDuckGo scraping was excluded earlier the same consultation (single
undocumented endpoint as a global SPOF + deliberate evasion of the
operator's bot detection). Architecture direction from the same
consultation: two thin Horizon-owned tools (`web_search`, `web_fetch`)
with swappable vendor adapters (shape B of section 5); Exa is the first
search adapter; fetch is own plain-HTTP extraction (no JS rendering
initially — a future embedded-browser viewer engine, e.g. Servo/wry,
could later back a JS-capable fetch adapter); **Brave** is the
documented second-adapter candidate (independent #1 overall; untested
by us only for lack of a key).

Known Exa caveats to carry into implementation: API churn (endpoint
retirements and field deprecations through H1 2026 — pin and watch the
changelog), SSE-framed responses on the hosted MCP endpoint (one extra
parse step if that route is used instead of REST), and the
REST-vs-hosted-MCP route choice itself (REST + API key is the
contract-clear option; signup credit figures were inconsistent across
sources — verify at signup time).

Still open (next consultation): approval/trust-boundary design — whether
`web_search`/`web_fetch` become the judge's first real BoundaryCrossing
customer, and whether they sit behind a crush-`agentic_fetch`-style
delegated subagent or as direct top-level tools.
