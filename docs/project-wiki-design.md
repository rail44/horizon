# Project wiki — design

Status: shape decided with the owner 2026-11-07/08; concept grouping
spiked and accepted 2026-11-08 ("悪くないと思うのでこれで進めてみましょう");
a pilot of the overview and three concept pages accepted the same day
("よい感じだと思います"); all 21 concept pages written by the session
agent the same day, read against local main a3521c17. Background reading: Anthropic's "The
components of agent memory" (2026-11-06) on wikis built by dreaming.

## Problem

The owner, 2026-11-07:

> docsはそもそも人間が読みやすい形式や粒度ではない
> 現状のプロジェクトの状態を眺め、次に手を入れる箇所の探索や知識を得るため

`docs/` records decisions; it is not a way to look over what the project
is now, find where to work next, or learn an area. The wiki is that view,
written for a person. It is separate from the knowledge layer
(`agent-knowledge-design.md`), which holds lessons for agents.

## Decisions

1. **Shape: an overview page plus one page per concept, linked to each
   other** (owner: "D"). A concept is a unit a person thinks in — sandbox,
   approval, the terminal — which usually spans several crates.
2. **Pages describe current behavior only** (owner: "a"). Design decisions
   and in-flight work are not restated: a page links to the `docs/` record
   or board item instead ("bやcはそれこそリンクでよい").
3. **The store is user-side Markdown** (owner: "iii"), like the knowledge
   layer: under `$XDG_DATA_HOME/horizon/`, outside the repository, so
   regenerating pages makes no commits.
4. **Analysis tooling is built where it is missing.** The owner wants the
   namespaces and the dependencies that should become links analysed by a
   mechanism, not guessed. The code database (`code-database-design.md`)
   already provides namespaces and the weighted namespace graph from SCIP;
   the missing part was grouping namespaces into concepts.
5. **Concepts come from clustering plus an agent pass** (owner: combine
   "b" and "c"): community detection on the namespace graph proposes
   groups; an agent names, splits, merges and moves them and writes the
   result down as data.

## The concept grouping spike (2026-11-08)

`duckdb-ext/cluster-spike/` (README, `results/findings.md`,
`results/concepts.md`). Input: 443 namespaces and 2068 undirected edges
weighted by non-test unit edges.

- leiden-rs (Leiden, modularity, resolution 2) proposed 18 groups, most of
  which read as one concept; the agent pass turned them into 21 concepts,
  moving 56 namespaces. Modularity fell 0.65 → 0.60: reference cohesion
  traded for meaning. The lowest-cohesion concepts are the cross-cutting
  ones the graph cannot see as a unit (shared UI, approval, prompt context,
  logd, preview).
- Every implementation tried is reproducible for a fixed seed, but
  leiden-rs's partitions move with the seed: against other seeds, 12–36
  more namespaces fall outside their concept's source group.
- **Consequence: the concept file is what persists, not the clustering.**
  Re-clustering and redoing the agent pass would re-argue those namespaces
  on seed noise. Clustering is kept for placing namespaces that are new
  since the file was written and for flagging concepts whose cohesion drops.

The owner accepted the 21 concepts as the starting page set.

## The page pilot (2026-11-08)

Written by the session agent into
`$XDG_DATA_HOME/horizon/wiki/<sanitized-root>/` (the knowledge layer's
root naming): `index.md`, `sandbox.md`, `approval.md`, `terminal.md`. The
three concepts were picked for differing shapes: a cohesive one, a
cross-cutting low-cohesion one, and one split across a daemon, a library
and a pane.

Page format, accepted with the pilot:

- Frontmatter: `concept`, `title`, `code_at` (the commit the page was read
  against), `written`.
- A concept page: what it is (a few lines); entry points (where users and
  other concepts touch it); the flow inside, naming the main types and
  functions with their files; links to other concepts with the
  concept-graph weight; related records (docs, board items — open ones
  apart from past ones — and knowledge-layer entries, links only); stale
  text noticed while reading; and the page's namespaces as its anchors.
- The overview: the process split, the concepts grouped by where they run
  with one line each (unwritten pages marked), the strongest concept-graph
  edges, and the entry records (`AGENTS.md`, the roadmap, the UX
  principles). It says the graph shows references only, not messages
  between processes.
- Japanese, since the reader is the owner.
- Pages are read against the owner's local `main`, not a worker branch:
  the pilot's branch was seven commits behind, two of them in approval.

What the pilot showed:

- Concept-graph weights and docs/board candidates were mechanical (a join
  of the concept file with `namespace_graph`, a grep of `docs/` for the
  concept's paths, the board's folded items); the body still needed the
  code read, and three statements had to be corrected against it before
  they were right.
- Reading for a page surfaces stale text, which serves the "find where to
  work next" purpose directly: in the pilot, a module comment claiming the
  PTY threads live in the shell, the roadmap still calling logd "not
  started", and a knowledge-layer entry citing a function main removed.

## Next directions (owner, 2026-11-08)

The owner named three: updating the wiki as main changes, asking an agent
about any spot in the wiki by comment, and embedding diagrams such as
mermaid. No order among them ("特にどれから、というのはないです").
Updating stays manual for now, since automatic and manual updating share
almost all their parts ("まずは手動運用でシンプルな形でいいかなと感じています").
Diagrams are deferred; gpui-component (7591a35) has no mermaid rendering.

## Comments and the wiki view (decided 2026-11-08)

- **A view of its own.** A wiki view built from the Markdown view
  (`src/markdown/`), with comments as what makes it specific to reading
  the wiki ("明らかに、コメント機能だと思うんですが").
- **A comment targets a selected text range**, and answers come back as a
  thread. Whoever answers — the thread's session or an ordinary one — can
  edit and create pages ("スレッドとして帰ってくれば十分。ただしそこで
  動作するセッションでも普段のセッションでも、必要に応じてページの修正や
  作成は出来る作りであるべき").
- **Agents are woken the way the board wakes them, but the board is not
  the store** ("boardと同様のエージェント発火機構を用いていいという意味で、
  boardをそのまま活用するという意味ではないです"). Comment threads live in
  a log of their own beside the wiki pages.
- **One session per thread.** A thread's session is closed after it
  answers and resumed when the thread gets a new message. Nothing about
  the session is shown to the reader: the thread itself shows whether a
  question is still unanswered ("利用者から見える状態はそもそも必要ない
  んじゃないですかね"). Closing a thread's session is internal and must not
  reuse `Terminated`, which is the owner's explicit, destructive end.
  `resume_session` today re-reads the whole event log (~640 MB) per call;
  resuming per follow-up needs a projection-based path like startup
  resume's.
- **Thread sessions do not change the repository**, for now; usage may
  change that ("一旦、リポジトリを変えることはない前提でよいですが、
  使用感次第で変わりウルトは思います").
- **Pages are written through dedicated tools**, both a partial edit
  (exact-string replacement, as `fs.edit`) and a whole-page write for new
  or regenerated pages. The store sits outside every workspace, so
  without them every write needs an approval.
- **Anchors borrow the W3C Web Annotation Data Model** (Recommendation,
  2017-02-23), a subset of it, kept conformant to its MUSTs:
  - Annotation core (`@context` `http://www.w3.org/ns/anno.jsonld`, `id`,
    `type`, `motivation`, `body`, `target`, `creator`, `created`,
    `modified`); `TextualBody` (`text/markdown`, `ja`); `SpecificResource`
    with `TextQuoteSelector` (primary) and `TextPositionSelector`
    (disambiguation) as alternatives; `TimeState` with `sourceDate` and
    `cached` pointing at a snapshot of the page when the comment was made;
    agents as `Person` (owner) and `Software` (a session).
  - A reply is an Annotation with motivation `replying` whose target is
    the parent Annotation. The spec has no reply example; a target may be
    any resource.
  - Quotes and positions are taken over the page's **logical text**, not
    its Markdown source — the owner wants logical positions kept, as HTML
    annotators anchor against the document tree's text rather than the
    HTML source ("位置の基準については論理的な位置を維持したいのが本音
    ではあります"). The logical text is defined once, in the wiki crate,
    from the `markdown` crate's syntax tree (`to_mdast`, the parser
    gpui-component's `TextView` also uses, whose nodes carry source
    offsets): the values of its text nodes in order, with a separator
    between blocks, plus a map from each logical character to its source
    offset. The view converts through source positions, so its own
    rendered string need not match: a selection's
    `selected_source_range()` maps to a logical range, and a stored
    logical range maps back to a source range and then, with
    `RenderedText::range_for_source()`, to a rendered range for
    `set_range_highlights()` (all public on `TextViewState` at
    gpui-kit 7591a35). Agents build the same logical text from the source
    with no gpui dependency. Positions count Unicode code points. Still
    to pin: the block separator, the character map inside text nodes that
    contain escapes, and parse options identical to the view's (GFM
    extensions), or the trees diverge.
  - Every identifier is an IRI (the Annotation `id` MUST be one;
    relative references such as `sandbox.md` are not).
  - Not used: the HTTP protocol, collections and paging, JSON-LD
    processing. The data is read and written as plain JSON carrying the
    `@context` line.
  - Why this subset: offsets alone break on any edit; edit-tracked
    positions (Yjs-style) need every write to pass one model and lose a
    comment when its own range is replaced, which agent edits do often;
    quote plus context with fuzzy re-anchoring is what annotation systems
    over documents they do not own use (Hypothesis), and the comment-time
    snapshot keeps an "outdated" display possible (GitHub review style).
    Web Annotation's broad web-wide vision did not take hold (no browser
    shipped it, Apache Annotator retired from the Incubator 2025-08-11),
    but its selector model is in use where one party holds documents and
    annotations (IIIF Presentation 3.0, EPUB Annotations drafts).
  - Conformance can be checked with the WPT `annotation-model/` JSON
    Schemas (draft-04, BSD-3) under a JSON Schema validator in tests.
    Re-anchoring can port Hypothesis's `match-quote.ts` (BSD-2) and the
    approximate search it uses (`approx-string-match`, MIT); no Rust crate
    implements either.
- **The wiki is an extension package**: its store, annotation model,
  anchoring, tools, skill and the shell's wiki view are kept apart from
  Horizon's core, positioned as an extension ("horizonの基本機能という
  より拡張という立ち位置を明確にしたい"). Roles and skills already have
  external registration (`roles::register_external`,
  `skills::register_external_skill_sources`); tools do not, so this work
  includes a seam for registering tools from outside `horizon-agent`.
  Moving the board's and knowledge layer's tools onto that seam is handed
  to a separate session directly, not through the board.

- **The comment log is one append-only file per page, written by a
  library in the wiki package** ("ページごとに1つのファイルに追記する形で",
  after "wiki の拡張の中に書き込みを担う部分を持たせるのが自然な気がします").
  Both writers — the shell's view and agentd writing a reply back — call
  the same function, which appends one record under an exclusive `flock`.
  Comment ids are generated by the writer (UUID IRIs), so nothing needs
  folding under the lock, unlike board writes. `horizon-logd` is not used:
  its wire, writer and poke name are board-specific and its wire lives in
  `horizon-board`, so wiki types there would make the board depend on the
  extension. New comments are noticed by watching or polling the files.

### What the investigation found (2026-11-08)

- **Waking (decided 2026-11-08, "それでよいです"): generalize `board_flow`.** Its
  outbox half (tailing the agent event log, pending answers and sends,
  per-recipient ordering, receipts) is already generic; its trigger half
  (folding the board log, the delivery cursor stored in the board log,
  owner messages to task sessions) is board-only. `ReplyAddress` is a
  closed, board-only enum and the board runner parses every `reply_to` in
  the log, so a wiki reply address would error there on every tick
  (`board_flow/dispatch.rs:160-164`): a parallel wiki flow would still
  have to change the board code, and would read the agent log a second
  time. The outbox is extracted into a core with trigger sources and
  reply sinks registered from outside, board and wiki each plugging in
  one. Risk: board delivery's retry and ordering invariants, covered by
  its existing tests.
- **No resumable close exists yet.** The only way to stop a session is
  `Shutdown`, which persists `Terminated`; startup skips dead sessions and
  `resume_session` revives them anyway. A thread session's close needs a
  new command and a persisted marker distinct from `Terminated`, which
  startup also skips. `SessionState` is not on the ACP-ext wire.
- **Whole-log reads per call**: `resume_session`, `acknowledge_delivery`
  (`session/resume.rs:572-584`) and the board's session operations read the
  entire event log each time — one full read per delivered reply. They
  need the projection-based path startup resume already has.
- **Runtime-approved network domains are not restored on resume**
  (approval adds them to the in-memory allowlist; resume seeds only the
  configured `[grants]`), so a resumed thread session may have to ask
  again before fetching the web. Confirmed as far as the in-memory add;
  the absence of a replay is the investigating agent's finding.

### Package layout (decided 2026-11-09)

- **`crates/horizon-wiki`**, a new domain crate depending on neither
  `horizon-agent` nor gpui (the board crate's boundary): the store and its
  paths, the concept file, page reads and writes (whole and partial),
  the comment log (Web Annotation types, `flock` append, thread
  assembly), the logical text and its source map, re-anchoring, and
  snapshots. The `project-wiki` skill and the thread-answering role move
  into it as constant data, as `horizon-board`'s `agents.rs` does.
  `cluster-spike`'s `place` and `concept-graph` move into it; the rest of
  the spike is deleted (owner: "消してよいです").
- **A tool registration seam in `horizon-agent`**: tools registered from
  outside reach the four places tool ids are matched today (the
  definitions in `tools/catalog.rs`, dispatch in `tools/execution.rs` and
  `tools/synchronous.rs`, typed input in `tools/input`, classification in
  `policy.rs`). The executor's signature uses plain types (a session id,
  JSON in and out), so `horizon-wiki` implements its tools without
  depending on `horizon-agent`. A registering package declares a tool
  contained, and the core trusts that declaration — the wiki's writes are
  confined to its store by the library. `horizon-agentd` is the
  composition root that registers the wiki's tools, role, skill, trigger
  source and reply sink (`docs/board-keeper-design.md` §3).
- **The shell's view is `ViewKind::Wiki`, with the view in `src/wiki/`**,
  the board's shape. This and the trusted declaration above are interim,
  until plugin infrastructure exists ("Aでよいですが、プラグインの整備まで
  一旦という印象ではあります"; "これも同上でその形でよいです"). The
  alternatives were a hook in the Markdown view through which an extension
  adds behavior, and registering view kinds from outside, which overlaps
  `docs/view-runtime-principle.md`'s plugin views as a third runtime.
- **Every identifier is a `urn:uuid:` IRI** (owner: "OK"): a page carries
  `id: urn:uuid:…` in its frontmatter, so a rename keeps its identity;
  snapshots are named by their uuid; a session is
  `urn:uuid:<session id>`; the owner is one uuid per installation. `tag:`
  URIs need a domain or email address the minter holds (RFC 4151), and
  `file://` IRIs tie identity to one machine's paths.

### Still open before planning

- The resumable close and the projection-based resume (design detail,
  no owner decision expected).
- The re-anchoring implementation (a port of Hypothesis's
  `match-quote.ts` and approximate search, or existing crates), now over
  the logical text.
- Later: how orphaned and outdated comments look; what else the view
  shows.

## Open points

- **Regenerating pages: settled 2026-11-08.** The procedure is the
  repository skill `.horizon/skills/project-wiki/SKILL.md`, written so it
  does not depend on which agent runs it (owner: "スキルとしてはエージェント
  実装によらない形でいいんじゃないですか？"); `.claude/skills/project-wiki`
  links to the same file. Its mechanical steps are `cluster-spike place`
  (namespaces the concept file lacks, with the concept each is most tied
  to, and names the code no longer has) and `cluster-spike concept-graph`
  (directed weights between concepts). The concept file moved to the store
  as `concepts.md` beside the pages (owner's choice), so open point "Where
  the concept file lives" is closed. Still open: a Horizon agent needs an
  approval to write to the store, which sits outside every workspace; a
  dedicated write path like `knowledge.write` would remove that, and
  whether one is wanted is undecided. The full pass found four more stale
  module comments beyond the pilot's, fixed on the same branch.
- **Choosing links to docs and the board.** The pilot used path mentions
  and the agent's judgment together. 50 of 72 docs mention repository
  paths; 53 of their 153 distinct backticked paths no longer exist.
- **Keeping pages current.** Anchoring pages to their concept's namespaces
  makes "did the code under this page change" a git question, as in the
  knowledge layer's verification pass.
- **The production clustering.** A crate inside code-db (leiden-rs or
  graphrs's Louvain; graphrs gives the same partition for every seed,
  which suits a candidate generator) versus keeping it a separate tool.
  Two DuckDB community extensions offer Louvain (petgraph_ext, onager);
  duckpgq offers none.
- **code-db friction met on the way.** It is not built by default, and
  `code-db index` requires jscpd even when only references are wanted.
