# cluster-spike

A spike, not a tool: it compares community-detection crates on the code
database's namespace graph, as the candidate-generation half of grouping
namespaces into concept pages (the other half is an agent naming, merging and
splitting the candidates, kept as data). It is a Cargo workspace of its own,
outside `check.sh`, and can be deleted whole once the spike is decided.

## Input

The namespace graph `code-db` already computes (`../sql/queries/namespace_graph.sql`):

```sh
code-db index REPO
code-db query REPO namespace_graph FORMAT=csv > edges.csv
```

`src/graph.rs` keeps an edge's unit edges that have no test unit at either end
(`unit_edges - test_unit_edges`), drops edges left with none, and sums the two
directions of a pair into one undirected weight.

## Running

```sh
cargo run --release -- sweep edges.csv            # every algorithm over resolutions and seeds
cargo run --release -- run edges.csv --algo leiden --resolution 2 --seed 1
cargo run --release -- concepts edges.csv results/concepts.md --algo leiden --resolution 2 --seed 1
```

`concepts` checks a concept file (`## id: title` headings, namespaces under
each) covers every namespace once, and compares it with a partition.
`results/` holds the agent pass's concept file as the spike left it, and the
findings; the live concept file is the wiki store's `concepts.md`.

Two commands serve the wiki skill (`.horizon/skills/project-wiki/`):

```sh
cargo run --release -- place edges.csv concepts.md          # new and vanished namespaces
cargo run --release -- concept-graph edges.csv concepts.md  # directed weights between concepts
```

Algorithms: `leiden` and `leiden-cpm` (leiden-rs, modularity and the Constant
Potts Model), `louvain-leiden-rs` (leiden-rs with refinement skipped),
`louvain-graphrs` (graphrs's seeded Louvain; its Leiden takes no seed).
leiden-rs is built without default features, so its rayon path — documented
as possibly differing from the sequential one — is not compiled.

The sweep's modularity is computed in `src/metrics.rs` at γ = 1 for every row,
and seed stability is the mean adjusted Rand index between the partitions of
different seeds.
