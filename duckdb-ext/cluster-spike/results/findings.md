# Findings (2026-11-08)

Input: this repository's namespace graph at d57c4efe (`code-db query REPO
namespace_graph FORMAT=csv`): 443 namespaces and 2068 undirected edges after
dropping test-only unit edges and summing both directions.

## Candidate generation (b)

`cluster-spike sweep`, seeds 1–5:

| algorithm | resolution | communities | largest | modularity | reproducible | seed stability (ARI) | ms per run |
|---|---|---|---|---|---|---|---|
| leiden | 1 | 15.0 | 77.6 | 0.6693 | yes | 0.768 | 1.3 |
| leiden | 2 | 19.0 | 61.4 | 0.6552 | yes | 0.796 | 1.4 |
| leiden | 4 | 31.8 | 33.2 | 0.5690 | yes | 0.835 | 1.4 |
| leiden-cpm | 0.2 | 41.6 | 87.0 | 0.6453 | yes | 0.848 | 1.2 |
| louvain-leiden-rs | 2 | 19.0 | 61.8 | 0.6550 | yes | 0.770 | 0.9 |
| louvain-graphrs | 2 | 20.0 | 63.0 | 0.6539 | yes | 1.000 | 8.2 |

(The full sweep also ran resolutions 0.5 and CPM 0.05/1/3.)

- Every implementation reproduces its partition for a repeated seed.
- leiden-rs's partitions move with the seed (ARI 0.77–0.93 between seeds);
  graphrs's Louvain gives the same partition for every seed tried.
- CPM leaves many singletons from resolution 0.2 up (12 at 0.2, 49 at 1),
  since γ is a density in weight units and most namespaces are lightly tied.
- Modularity resolution 2 gives 18–20 communities of which most read as one
  concept each (workspace model, terminal, board, sandbox, config and theme,
  agent views, event-log persistence, decision model and judge, CLI and
  control plane, code-db and its extensions).

## The agent pass (c)

`results/concepts.md`: 21 concepts over leiden resolution=2 seed=1, every
namespace assigned exactly once (`cluster-spike concepts` checks it).

- 56 of 443 namespaces were moved out of their concept's dominant cluster:
  splitting the 72-namespace agent-tools cluster (prompt context, approval
  routing, the sandbox proxy), splitting config from theme, moving the
  command palette (`src/palette.rs`) out of theme, gathering the preview pane,
  separating the shared daemon wire and runtime clients from the terminal and
  board clusters, and merging the three code-db clusters.
- Modularity 0.6521 → 0.6009 and ARI 0.736 against the source partition: the
  concepts give up some reference cohesion for meaning.
- The concepts with the lowest cohesion are the cross-cutting ones the graph
  cannot see as a unit: shared-ui 0.10, approval 0.20, prompt-context 0.26,
  logd 0.28, preview 0.30.

## Stability of the pair

The same concept file against other partitions:

| partition | ARI with the concepts | namespaces outside their concept's dominant cluster |
|---|---|---|
| leiden r=2 seed 1 (the source) | 0.736 | 56 |
| leiden r=2 seeds 2–5 | 0.605–0.700 | 68–92 |
| louvain-graphrs r=2, any seed | 0.656 | 90 |

Against another seed, 12–36 more namespaces fall outside their concept's
dominant cluster than against the source seed, so re-running (b) and redoing
(c) from its output would re-litigate that many on seed noise alone. The concept file, not the clustering, has to
be what persists; the clustering is better used to place namespaces that are
new since the file was written and to flag concepts whose cohesion drops.

## Not checked

- Directed edges (the input is symmetrised on purpose).
- How the agent pass performs when run by Horizon's production model rather
  than in this session.
- Edge weights other than non-test unit edges (occurrence counts).
