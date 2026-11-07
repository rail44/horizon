//! The compared implementations, each turning a graph into one community
//! index per node.

use crate::graph::NamespaceGraph;
use crate::metrics::renumber;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// leiden-rs, Leiden over modularity (resolution γ, 1.0 = classic).
    LeidenModularity,
    /// leiden-rs, Leiden over the Constant Potts Model (γ is a density in
    /// weight units, so its scale differs from modularity's).
    LeidenCpm,
    /// leiden-rs with the refinement phase skipped: Louvain-like.
    LouvainLeidenRs,
    /// graphrs's seeded Louvain.
    LouvainGraphrs,
}

impl Algorithm {
    pub const ALL: [Algorithm; 4] = [
        Algorithm::LeidenModularity,
        Algorithm::LeidenCpm,
        Algorithm::LouvainLeidenRs,
        Algorithm::LouvainGraphrs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Algorithm::LeidenModularity => "leiden",
            Algorithm::LeidenCpm => "leiden-cpm",
            Algorithm::LouvainLeidenRs => "louvain-leiden-rs",
            Algorithm::LouvainGraphrs => "louvain-graphrs",
        }
    }

    pub fn parse(name: &str) -> Option<Algorithm> {
        Algorithm::ALL.into_iter().find(|a| a.name() == name)
    }

    pub fn uses_cpm(self) -> bool {
        self == Algorithm::LeidenCpm
    }
}

/// One community index per node, renumbered in order of first appearance so
/// equal partitions compare equal.
pub fn run(
    algorithm: Algorithm,
    graph: &NamespaceGraph,
    resolution: f64,
    seed: u64,
) -> Result<Vec<usize>, String> {
    let membership = match algorithm {
        Algorithm::LeidenModularity => leiden_rs_run(
            graph,
            resolution,
            seed,
            leiden_rs::QualityType::Modularity,
            false,
        ),
        Algorithm::LeidenCpm => {
            leiden_rs_run(graph, resolution, seed, leiden_rs::QualityType::CPM, false)
        }
        Algorithm::LouvainLeidenRs => leiden_rs_run(
            graph,
            resolution,
            seed,
            leiden_rs::QualityType::Modularity,
            true,
        ),
        Algorithm::LouvainGraphrs => graphrs_louvain(graph, resolution, seed),
    }?;
    Ok(renumber(&membership))
}

fn leiden_rs_run(
    graph: &NamespaceGraph,
    resolution: f64,
    seed: u64,
    quality: leiden_rs::QualityType,
    skip_refinement: bool,
) -> Result<Vec<usize>, String> {
    let mut builder = leiden_rs::GraphDataBuilder::new(graph.node_count());
    for &(u, v, weight) in &graph.edges {
        builder
            .add_edge(u, v, weight)
            .map_err(|e| format!("leiden-rs edge ({u}, {v}): {e}"))?;
    }
    let data = builder
        .build()
        .map_err(|e| format!("leiden-rs graph: {e}"))?;
    let config = leiden_rs::LeidenConfig::builder()
        .resolution(resolution)
        .seed(seed)
        .quality(quality)
        .skip_refinement(skip_refinement)
        .build();
    let output = leiden_rs::Leiden::new(config)
        .run(&data)
        .map_err(|e| format!("leiden-rs run: {e}"))?;
    Ok(output.partition.as_slice().to_vec())
}

fn graphrs_louvain(
    graph: &NamespaceGraph,
    resolution: f64,
    seed: u64,
) -> Result<Vec<usize>, String> {
    use graphrs::algorithms::community::louvain::louvain_communities;
    use graphrs::{Edge, Graph, GraphSpecs, Node};

    let nodes = (0..graph.node_count()).map(Node::from_name).collect();
    let edges = graph
        .edges
        .iter()
        .map(|&(u, v, weight)| Edge::with_weight(u, v, weight))
        .collect();
    let g = Graph::<usize, ()>::new_from_nodes_and_edges(nodes, edges, GraphSpecs::undirected())
        .map_err(|e| format!("graphrs graph: {e:?}"))?;
    let communities = louvain_communities(&g, true, Some(resolution), None, Some(seed))
        .map_err(|e| format!("graphrs louvain: {e:?}"))?;
    let mut membership = vec![usize::MAX; graph.node_count()];
    // Order the communities by their smallest node so the output does not
    // depend on how the Vec of HashSets was assembled.
    let mut ordered: Vec<Vec<usize>> = communities
        .into_iter()
        .map(|set| {
            let mut nodes: Vec<usize> = set.into_iter().collect();
            nodes.sort_unstable();
            nodes
        })
        .collect();
    ordered.sort();
    for (community, nodes) in ordered.iter().enumerate() {
        for &node in nodes {
            membership[node] = community;
        }
    }
    if membership.contains(&usize::MAX) {
        return Err("graphrs louvain left a node without a community".to_string());
    }
    Ok(membership)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{adjusted_rand_index, modularity};

    /// Two 5-cliques joined by one light edge.
    fn two_cliques() -> NamespaceGraph {
        let mut edges = Vec::new();
        for offset in [0, 5] {
            for u in 0..5 {
                for v in (u + 1)..5 {
                    edges.push((offset + u, offset + v, 3.0));
                }
            }
        }
        edges.push((4, 5, 1.0));
        edges.sort_by(|a, b| a.partial_cmp(b).unwrap());
        NamespaceGraph {
            names: (0..10).map(|i| format!("n{i}")).collect(),
            packages: vec!["p".to_string(); 10],
            edges,
        }
    }

    #[test]
    fn every_modularity_algorithm_separates_the_cliques() {
        let graph = two_cliques();
        let expected = vec![0, 0, 0, 0, 0, 1, 1, 1, 1, 1];
        for algorithm in Algorithm::ALL.into_iter().filter(|a| !a.uses_cpm()) {
            let membership = run(algorithm, &graph, 1.0, 7).unwrap();
            assert_eq!(membership, expected, "{}", algorithm.name());
        }
    }

    #[test]
    fn cpm_separates_the_cliques_at_a_density_between_the_two() {
        // Inside a clique the weight per pair is 3; across, 1/25.
        let membership = run(Algorithm::LeidenCpm, &two_cliques(), 1.0, 7).unwrap();
        assert_eq!(membership, vec![0, 0, 0, 0, 0, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn the_same_seed_gives_the_same_partition() {
        let graph = two_cliques();
        for algorithm in Algorithm::ALL {
            let first = run(algorithm, &graph, 1.0, 11).unwrap();
            let second = run(algorithm, &graph, 1.0, 11).unwrap();
            assert_eq!(first, second, "{}", algorithm.name());
        }
    }

    #[test]
    fn the_found_partition_scores_its_expected_modularity() {
        let graph = two_cliques();
        let membership = run(Algorithm::LeidenModularity, &graph, 1.0, 3).unwrap();
        let other = run(Algorithm::LouvainGraphrs, &graph, 1.0, 3).unwrap();
        assert_eq!(adjusted_rand_index(&membership, &other), 1.0);
        // m = 61, each side holds 30 of it with degree sum 61.
        let q = modularity(&graph, &membership, 1.0);
        let expected = 2.0 * (30.0 / 61.0 - 0.25);
        assert!((q - expected).abs() < 1e-12, "{q} vs {expected}");
    }
}
