# Project wiki — design

Status: shape decided with the owner 2026-11-07/08; concept grouping
spiked and accepted 2026-11-08 ("悪くないと思うのでこれで進めてみましょう");
no page has been generated yet. Background reading: Anthropic's "The
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

## Open points

- **Page format and generation.** What a concept page contains (entry
  points, the flow through the code, public surface, links), and how the
  agent pass that writes pages is run — presumably a skill on request, as
  distillation is (`horizon-distill`).
- **Choosing links to docs and the board.** Candidates can come from the
  repository paths a doc or board item mentions (50 of 72 docs mention
  paths; 53 of their 153 distinct backticked paths no longer exist), from
  an agent reading them, or both.
- **Keeping pages current.** Anchoring pages to their concept's namespaces
  makes "did the code under this page change" a git question, as in the
  knowledge layer's verification pass.
- **Where the concept file lives.** In the spike it is in the repository;
  decision 3 puts pages user-side, and the concept file may follow them.
- **The production clustering.** A crate inside code-db (leiden-rs or
  graphrs's Louvain; graphrs gives the same partition for every seed,
  which suits a candidate generator) versus keeping it a separate tool.
  Two DuckDB community extensions offer Louvain (petgraph_ext, onager);
  duckpgq offers none.
- **code-db friction met on the way.** It is not built by default, and
  `code-db index` requires jscpd even when only references are wanted.
