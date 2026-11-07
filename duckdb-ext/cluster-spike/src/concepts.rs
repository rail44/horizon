//! The agent pass's output: concepts as data (`## id: title` headings, one
//! namespace per line under each), checked against the graph and compared
//! with the partition it started from.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use crate::graph::NamespaceGraph;
use crate::metrics::{adjusted_rand_index, modularity};

#[derive(Debug, Clone, PartialEq)]
pub struct Concept {
    pub id: String,
    pub title: String,
    pub namespaces: Vec<String>,
}

/// Reads the concept file. A line starting with `## ` opens a concept; a
/// non-empty line that does not start with `#` and comes after one is a
/// namespace; everything else is prose and ignored.
pub fn parse_concepts(text: &str) -> Result<Vec<Concept>, String> {
    let mut concepts: Vec<Concept> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if let Some(heading) = line.strip_prefix("## ") {
            let (id, title) = heading
                .split_once(':')
                .ok_or_else(|| format!("line {}: expected `## id: title`", number + 1))?;
            concepts.push(Concept {
                id: id.trim().to_string(),
                title: title.trim().to_string(),
                namespaces: Vec::new(),
            });
        } else if !line.is_empty()
            && !line.starts_with('#')
            && let Some(concept) = concepts.last_mut()
            && looks_like_a_namespace(line)
        {
            concept.namespaces.push(line.to_string());
        }
    }
    Ok(concepts)
}

/// Prose between headings is allowed; a namespace is a path without spaces.
fn looks_like_a_namespace(line: &str) -> bool {
    !line.contains(' ') && (line.contains('/') || line.contains("::"))
}

/// Every graph namespace in exactly one concept, and no concept naming a
/// namespace the graph does not have. Returns one concept index per node.
pub fn assign(graph: &NamespaceGraph, concepts: &[Concept]) -> Result<Vec<usize>, String> {
    let index: HashMap<&str, usize> = graph
        .names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let mut membership = vec![usize::MAX; graph.node_count()];
    let mut problems = Vec::new();
    for (c, concept) in concepts.iter().enumerate() {
        for name in &concept.namespaces {
            match index.get(name.as_str()) {
                None => problems.push(format!("{}: unknown namespace {name}", concept.id)),
                Some(&node) if membership[node] != usize::MAX => problems.push(format!(
                    "{name}: in both {} and {}",
                    concepts[membership[node]].id, concept.id
                )),
                Some(&node) => membership[node] = c,
            }
        }
    }
    for (node, &c) in membership.iter().enumerate() {
        if c == usize::MAX {
            problems.push(format!("unassigned namespace {}", graph.names[node]));
        }
    }
    if problems.is_empty() {
        Ok(membership)
    } else {
        Err(problems.join("\n"))
    }
}

/// The comparison of the concepts with the algorithm partition they started
/// from: modularity of both, their adjusted Rand index, how many namespaces
/// sit outside their concept's dominant source cluster, and per concept its
/// cohesion (the share of its edge weight that stays inside it).
pub fn comparison(
    graph: &NamespaceGraph,
    concepts: &[Concept],
    concept_of: &[usize],
    cluster_of: &[usize],
) -> String {
    let mut weight_inside = vec![0.0; concepts.len()];
    let mut weight_touching = vec![0.0; concepts.len()];
    for &(u, v, w) in &graph.edges {
        let (a, b) = (concept_of[u], concept_of[v]);
        weight_touching[a] += w;
        if a == b {
            weight_inside[a] += w;
        } else {
            weight_touching[b] += w;
        }
    }

    let mut moved = 0;
    let mut rows = String::new();
    for (c, concept) in concepts.iter().enumerate() {
        let members: Vec<usize> = (0..graph.node_count())
            .filter(|&n| concept_of[n] == c)
            .collect();
        let mut sources: BTreeMap<usize, usize> = BTreeMap::new();
        for &n in &members {
            *sources.entry(cluster_of[n]).or_default() += 1;
        }
        let (dominant, from_dominant) = sources
            .iter()
            .max_by_key(|(cluster, count)| (**count, std::cmp::Reverse(**cluster)))
            .map(|(cluster, count)| (*cluster, *count))
            .unwrap_or((0, 0));
        moved += members.len() - from_dominant;
        let packages: BTreeSet<&str> = members
            .iter()
            .map(|&n| graph.packages[n].as_str())
            .collect();
        let cohesion = if weight_touching[c] > 0.0 {
            weight_inside[c] / weight_touching[c]
        } else {
            0.0
        };
        writeln!(
            rows,
            "| {} | {} | {} | {} | {cohesion:.2} | cluster {} ({from_dominant}) {} |",
            concept.id,
            concept.title,
            members.len(),
            packages.len(),
            dominant + 1,
            sources
                .iter()
                .filter(|(cluster, _)| **cluster != dominant)
                .map(|(cluster, count)| format!("+{count} from {}", cluster + 1))
                .collect::<Vec<_>>()
                .join(", "),
        )
        .unwrap();
    }

    let mut out = String::new();
    writeln!(
        out,
        "concepts: {}, algorithm clusters: {}; modularity {:.4} (concepts) vs {:.4} (clusters); \
         adjusted Rand index {:.3}; namespaces outside their concept's dominant cluster: {moved} of {}\n",
        concepts.len(),
        cluster_of.iter().max().map_or(0, |c| c + 1),
        modularity(graph, concept_of, 1.0),
        modularity(graph, cluster_of, 1.0),
        adjusted_rand_index(concept_of, cluster_of),
        graph.node_count(),
    )
    .unwrap();
    writeln!(
        out,
        "| concept | title | namespaces | packages | cohesion | from |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|").unwrap();
    out.push_str(&rows);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> NamespaceGraph {
        NamespaceGraph {
            names: vec!["a/x.rs".into(), "a/y.rs".into(), "b/z.rs".into()],
            packages: vec!["a".into(), "a".into(), "b".into()],
            edges: vec![(0, 1, 3.0), (1, 2, 1.0)],
        }
    }

    #[test]
    fn headings_open_concepts_and_prose_is_skipped() {
        let text = "# Title\nintro text here\n## one: First\nsome prose\na/x.rs\na/y.rs\n\n## two: Second\nb/z.rs\n";
        let concepts = parse_concepts(text).unwrap();
        assert_eq!(concepts.len(), 2);
        assert_eq!(concepts[0].id, "one");
        assert_eq!(concepts[0].title, "First");
        assert_eq!(concepts[0].namespaces, ["a/x.rs", "a/y.rs"]);
        assert_eq!(concepts[1].namespaces, ["b/z.rs"]);
    }

    #[test]
    fn assignment_reports_unknown_duplicate_and_missing_namespaces() {
        let concepts = parse_concepts("## one: A\na/x.rs\nc/w.rs\n## two: B\na/x.rs\n").unwrap();
        let error = assign(&graph(), &concepts).unwrap_err();
        assert!(error.contains("one: unknown namespace c/w.rs"), "{error}");
        assert!(error.contains("a/x.rs: in both one and two"), "{error}");
        assert!(error.contains("unassigned namespace a/y.rs"), "{error}");
        assert!(error.contains("unassigned namespace b/z.rs"), "{error}");
    }

    #[test]
    fn the_comparison_counts_moved_namespaces_and_cohesion() {
        let concepts = parse_concepts("## one: A\na/x.rs\na/y.rs\n## two: B\nb/z.rs\n").unwrap();
        let concept_of = assign(&graph(), &concepts).unwrap();
        // The algorithm put y with z; the concepts moved it to x.
        let report = comparison(&graph(), &concepts, &concept_of, &[0, 1, 1]);
        assert!(
            report.contains("namespaces outside their concept's dominant cluster: 1 of 3"),
            "{report}"
        );
        // one: 3 inside of 4 touching; two: 0 of 1.
        assert!(report.contains("| one | A | 2 | 1 | 0.75 |"), "{report}");
        assert!(report.contains("| two | B | 1 | 1 | 0.00 |"), "{report}");
    }
}
