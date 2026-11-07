//! The two outputs: one partition listed community by community, and a sweep
//! comparing algorithms over resolutions and seeds.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::Instant;

use crate::algorithms::{Algorithm, run};
use crate::graph::NamespaceGraph;
use crate::metrics::{adjusted_rand_index, modularity};

/// One partition: a summary line, then each community largest first with its
/// members and the packages they span.
pub fn partition_listing(
    graph: &NamespaceGraph,
    algorithm: Algorithm,
    resolution: f64,
    seed: u64,
) -> Result<String, String> {
    let membership = run(algorithm, graph, resolution, seed)?;
    let mut communities: Vec<Vec<usize>> =
        vec![Vec::new(); membership.iter().max().map_or(0, |c| c + 1)];
    for (node, &community) in membership.iter().enumerate() {
        communities[community].push(node);
    }
    communities.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    let mut out = String::new();
    writeln!(
        out,
        "# {} resolution={resolution} seed={seed}: {} namespaces, {} edges, {} communities, modularity {:.4}",
        algorithm.name(),
        graph.node_count(),
        graph.edges.len(),
        communities.len(),
        modularity(graph, &membership, 1.0),
    )
    .unwrap();
    for (rank, members) in communities.iter().enumerate() {
        let packages: BTreeSet<&str> = members
            .iter()
            .map(|&n| graph.packages[n].as_str())
            .collect();
        writeln!(
            out,
            "\n## {} ({} namespaces; packages: {})",
            rank + 1,
            members.len(),
            packages.into_iter().collect::<Vec<_>>().join(", ")
        )
        .unwrap();
        for &node in members {
            writeln!(out, "{}", graph.names[node]).unwrap();
        }
    }
    Ok(out)
}

/// One row per (algorithm, resolution): communities, sizes, modularity at
/// γ = 1 (comparable across rows), whether a repeated seed reproduces its
/// partition, and the mean adjusted Rand index between seeds (1 = the seed
/// does not matter).
pub fn sweep_table(
    graph: &NamespaceGraph,
    runs: &[(Algorithm, f64)],
    seeds: &[u64],
) -> Result<String, String> {
    let mut out = String::new();
    writeln!(
        out,
        "| algorithm | resolution | communities | largest | singletons | modularity | reproducible | seed stability (ARI) | ms per run |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|---|---|---|").unwrap();
    for &(algorithm, resolution) in runs {
        let mut partitions = Vec::new();
        let mut reproducible = true;
        let started = Instant::now();
        for &seed in seeds {
            let first = run(algorithm, graph, resolution, seed)?;
            let again = run(algorithm, graph, resolution, seed)?;
            reproducible &= first == again;
            partitions.push(first);
        }
        let per_run = started.elapsed().as_secs_f64() * 1000.0 / (2 * seeds.len()) as f64;
        let mean = |values: Vec<f64>| values.iter().sum::<f64>() / values.len().max(1) as f64;
        let sizes = |p: &Vec<usize>| {
            let mut sizes = vec![0usize; p.iter().max().map_or(0, |c| c + 1)];
            for &c in p {
                sizes[c] += 1;
            }
            sizes
        };
        let communities = mean(partitions.iter().map(|p| sizes(p).len() as f64).collect());
        let largest = mean(
            partitions
                .iter()
                .map(|p| *sizes(p).iter().max().unwrap_or(&0) as f64)
                .collect(),
        );
        let singletons = mean(
            partitions
                .iter()
                .map(|p| sizes(p).iter().filter(|&&s| s == 1).count() as f64)
                .collect(),
        );
        let q = mean(
            partitions
                .iter()
                .map(|p| modularity(graph, p, 1.0))
                .collect(),
        );
        let mut aris = Vec::new();
        for i in 0..partitions.len() {
            for j in (i + 1)..partitions.len() {
                aris.push(adjusted_rand_index(&partitions[i], &partitions[j]));
            }
        }
        let stability = if aris.is_empty() { 1.0 } else { mean(aris) };
        writeln!(
            out,
            "| {} | {resolution} | {communities:.1} | {largest:.1} | {singletons:.1} | {q:.4} | {} | {stability:.3} | {per_run:.1} |",
            algorithm.name(),
            if reproducible { "yes" } else { "no" },
        )
        .unwrap();
    }
    Ok(out)
}
