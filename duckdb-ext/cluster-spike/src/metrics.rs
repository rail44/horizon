//! Measures computed here rather than taken from either crate, so every
//! implementation is scored the same way.

use std::collections::HashMap;

use crate::graph::NamespaceGraph;

/// Community indices renumbered in order of first appearance.
pub fn renumber(membership: &[usize]) -> Vec<usize> {
    let mut seen: HashMap<usize, usize> = HashMap::new();
    membership
        .iter()
        .map(|c| {
            let next = seen.len();
            *seen.entry(*c).or_insert(next)
        })
        .collect()
}

/// Newman's weighted modularity of an undirected graph with a resolution
/// γ: Σ_c [ L_c / m − γ (d_c / 2m)² ], L_c the weight inside c, d_c its
/// degree sum, m the total weight.
pub fn modularity(graph: &NamespaceGraph, membership: &[usize], resolution: f64) -> f64 {
    let communities = membership.iter().max().map_or(0, |c| c + 1);
    let mut inside = vec![0.0; communities];
    let mut degree = vec![0.0; communities];
    let mut total = 0.0;
    for &(u, v, w) in &graph.edges {
        total += w;
        degree[membership[u]] += w;
        degree[membership[v]] += w;
        if membership[u] == membership[v] {
            inside[membership[u]] += w;
        }
    }
    if total == 0.0 {
        return 0.0;
    }
    (0..communities)
        .map(|c| inside[c] / total - resolution * (degree[c] / (2.0 * total)).powi(2))
        .sum()
}

/// Hubert and Arabie's adjusted Rand index: 1 for identical partitions,
/// around 0 for unrelated ones.
pub fn adjusted_rand_index(a: &[usize], b: &[usize]) -> f64 {
    assert_eq!(a.len(), b.len(), "partitions of different node sets");
    let pairs = |n: f64| n * (n - 1.0) / 2.0;
    let mut table: HashMap<(usize, usize), f64> = HashMap::new();
    let mut rows: HashMap<usize, f64> = HashMap::new();
    let mut columns: HashMap<usize, f64> = HashMap::new();
    for (&x, &y) in a.iter().zip(b) {
        *table.entry((x, y)).or_default() += 1.0;
        *rows.entry(x).or_default() += 1.0;
        *columns.entry(y).or_default() += 1.0;
    }
    let index: f64 = table.values().map(|&n| pairs(n)).sum();
    let row_sum: f64 = rows.values().map(|&n| pairs(n)).sum();
    let column_sum: f64 = columns.values().map(|&n| pairs(n)).sum();
    let expected = row_sum * column_sum / pairs(a.len() as f64);
    let max = (row_sum + column_sum) / 2.0;
    if max == expected {
        return 1.0;
    }
    (index - expected) / (max - expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renumbering_follows_first_appearance() {
        assert_eq!(renumber(&[7, 7, 2, 9, 2]), [0, 0, 1, 2, 1]);
    }

    #[test]
    fn identical_partitions_under_other_labels_score_one() {
        assert_eq!(adjusted_rand_index(&[0, 0, 1, 1], &[5, 5, 3, 3]), 1.0);
    }

    #[test]
    fn a_known_adjusted_rand_index() {
        // sklearn.metrics.adjusted_rand_score([0,0,1,1],[0,0,1,2]) = 0.5714285714285715
        let ari = adjusted_rand_index(&[0, 0, 1, 1], &[0, 0, 1, 2]);
        assert!((ari - 4.0 / 7.0).abs() < 1e-12, "{ari}");
    }

    #[test]
    fn one_community_has_zero_modularity() {
        let graph = NamespaceGraph {
            names: vec!["a".into(), "b".into(), "c".into()],
            packages: vec!["p".into(); 3],
            edges: vec![(0, 1, 1.0), (1, 2, 2.0)],
        };
        assert_eq!(modularity(&graph, &[0, 0, 0], 1.0), 0.0);
    }
}
