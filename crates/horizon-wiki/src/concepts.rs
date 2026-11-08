//! The concept file and the namespace graph it is checked against.
//!
//! The concept file (`concepts.md` in the store) assigns each namespace to a
//! concept page: a `## id: title` heading opens a concept, and the
//! namespaces under it are its lines; other lines are prose. It is what
//! persists; clustering only proposes (docs/project-wiki-design.md, "The
//! concept grouping spike"). The namespace graph is the CSV
//! `code-db query REPO namespace_graph FORMAT=csv` prints.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use crate::{Result, WikiError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Concept {
    pub id: String,
    pub title: String,
    pub namespaces: Vec<String>,
}

/// A directed dependency between two namespaces: its non-test unit edges.
#[derive(Debug, Clone, PartialEq)]
pub struct Arc {
    pub src: String,
    pub dst: String,
    pub weight: f64,
}

pub fn parse_concepts(text: &str) -> Result<Vec<Concept>> {
    let mut concepts: Vec<Concept> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if let Some(heading) = line.strip_prefix("## ") {
            let (id, title) = heading.split_once(':').ok_or_else(|| {
                WikiError::Invalid(format!("line {}: expected `## id: title`", number + 1))
            })?;
            concepts.push(Concept {
                id: id.trim().to_string(),
                title: title.trim().to_string(),
                namespaces: Vec::new(),
            });
            continue;
        }
        if line.is_empty() || line.starts_with('#') || !looks_like_a_namespace(line) {
            continue;
        }
        if let Some(concept) = concepts.last_mut() {
            concept.namespaces.push(line.to_string());
        }
    }
    Ok(concepts)
}

/// Prose between headings is allowed; a namespace is a path without spaces.
fn looks_like_a_namespace(line: &str) -> bool {
    !line.contains(' ') && (line.contains('/') || line.contains("::"))
}

/// The arcs of `namespace_graph`'s CSV, each ordered pair once, weighted by
/// its unit edges with no test unit at either end (`unit_edges -
/// test_unit_edges`); pairs with none are dropped.
pub fn parse_namespace_arcs(csv: &str) -> Result<Vec<Arc>> {
    let mut lines = csv.lines().filter(|line| !line.trim().is_empty());
    let header: Vec<&str> = lines
        .next()
        .ok_or_else(|| WikiError::Invalid("empty namespace graph".into()))?
        .split(',')
        .collect();
    let column = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .ok_or_else(|| WikiError::Invalid(format!("missing column `{name}`")))
    };
    let (src, dst, edges, test_edges) = (
        column("src_module")?,
        column("dst_module")?,
        column("unit_edges")?,
        column("test_unit_edges")?,
    );
    let mut arcs: BTreeMap<(String, String), f64> = BTreeMap::new();
    for (row, line) in lines.enumerate() {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() != header.len() {
            return Err(WikiError::Invalid(format!(
                "row {}: {} fields, header has {}",
                row + 2,
                fields.len(),
                header.len()
            )));
        }
        let number = |at: usize| {
            fields[at]
                .parse::<u64>()
                .map_err(|e| WikiError::Invalid(format!("row {}: `{}`: {e}", row + 2, fields[at])))
        };
        let weight = number(edges)?.saturating_sub(number(test_edges)?);
        if weight > 0 {
            *arcs
                .entry((fields[src].to_string(), fields[dst].to_string()))
                .or_default() += weight as f64;
        }
    }
    Ok(arcs
        .into_iter()
        .map(|((src, dst), weight)| Arc { src, dst, weight })
        .collect())
}

/// Namespace → concept id (a namespace named twice keeps its first concept).
fn concept_by_namespace(concepts: &[Concept]) -> HashMap<&str, &str> {
    let mut map = HashMap::new();
    for concept in concepts {
        for name in &concept.namespaces {
            map.entry(name.as_str()).or_insert(concept.id.as_str());
        }
    }
    map
}

/// Directed weights between concepts as CSV `src,dst,weight`, largest first
/// within each source. Arcs touching a namespace the file does not name are
/// left out ([`placement`] lists those).
pub fn concept_graph(arcs: &[Arc], concepts: &[Concept]) -> String {
    let of = concept_by_namespace(concepts);
    let mut weights: BTreeMap<(&str, &str), f64> = BTreeMap::new();
    for arc in arcs {
        if let (Some(a), Some(b)) = (of.get(arc.src.as_str()), of.get(arc.dst.as_str())) {
            if a != b {
                *weights.entry((a, b)).or_default() += arc.weight;
            }
        }
    }
    let mut rows: Vec<((&str, &str), f64)> = weights.into_iter().collect();
    rows.sort_by(|((a1, b1), w1), ((a2, b2), w2)| {
        a1.cmp(a2).then(w2.total_cmp(w1)).then(b1.cmp(b2))
    });
    let mut out = String::from("src,dst,weight\n");
    for ((a, b), w) in rows {
        writeln!(out, "{a},{b},{w}").unwrap();
    }
    out
}

/// What keeping the concept file current needs: namespaces in the graph the
/// file does not name, each with the concept it is most strongly tied to
/// (arcs both ways), and names in the file the graph no longer has.
pub fn placement(arcs: &[Arc], concepts: &[Concept]) -> String {
    let of = concept_by_namespace(concepts);
    let present: BTreeSet<&str> = arcs
        .iter()
        .flat_map(|arc| [arc.src.as_str(), arc.dst.as_str()])
        .collect();
    let mut out = String::new();

    let missing: Vec<&str> = present
        .iter()
        .copied()
        .filter(|name| !of.contains_key(name))
        .collect();
    writeln!(
        out,
        "## namespaces not in the concept file: {}",
        missing.len()
    )
    .unwrap();
    for name in &missing {
        let mut ties: BTreeMap<&str, f64> = BTreeMap::new();
        for arc in arcs {
            let other = if arc.src == *name {
                &arc.dst
            } else if arc.dst == *name {
                &arc.src
            } else {
                continue;
            };
            if let Some(concept) = of.get(other.as_str()) {
                *ties.entry(concept).or_default() += arc.weight;
            }
        }
        let best = ties
            .iter()
            .max_by(|(c1, w1), (c2, w2)| w1.total_cmp(w2).then(c2.cmp(c1)));
        match best {
            Some((concept, weight)) => writeln!(out, "{name} -> {concept} ({weight})").unwrap(),
            None => writeln!(out, "{name} -> (no tie to a named namespace)").unwrap(),
        }
    }

    let mut gone: Vec<&str> = of
        .keys()
        .copied()
        .filter(|name| !present.contains(name))
        .collect();
    gone.sort_unstable();
    writeln!(
        out,
        "\n## namespaces in the concept file the graph no longer has: {}",
        gone.len()
    )
    .unwrap();
    for name in gone {
        writeln!(out, "{name} (in {})", of[name]).unwrap();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "src_module,dst_module,src_package,dst_package,unit_edges,occurrences,test_unit_edges,ambiguous_unit_edges,nested_modules,reverse_unit_edges";

    fn arcs() -> Vec<Arc> {
        let csv = format!(
            "{HEADER}\n\
             a/x.rs,a/y.rs,p,p,3,9,0,0,false,0\n\
             a/y.rs,b/z.rs,p,p,1,1,0,0,false,0\n\
             b/z.rs,a/x.rs,p,p,2,4,0,0,false,0\n\
             a/x.rs,tests.rs,p,p,5,5,5,0,false,0\n"
        );
        parse_namespace_arcs(&csv).unwrap()
    }

    #[test]
    fn headings_open_concepts_and_prose_is_skipped() {
        let concepts = parse_concepts(
            "# Concepts\nintro text\n## one: First\nsome prose\na/x.rs\n\n## two: Second\nb/z.rs\n",
        )
        .unwrap();
        assert_eq!(concepts.len(), 2);
        assert_eq!(concepts[0].title, "First");
        assert_eq!(concepts[0].namespaces, ["a/x.rs"]);
        assert_eq!(concepts[1].namespaces, ["b/z.rs"]);
    }

    #[test]
    fn test_only_arcs_drop() {
        assert_eq!(arcs().len(), 3);
        assert!(arcs().iter().all(|arc| arc.dst != "tests.rs"));
    }

    #[test]
    fn the_concept_graph_sums_directed_arcs_between_concepts() {
        let concepts = parse_concepts("## one: A\na/x.rs\na/y.rs\n## two: B\nb/z.rs\n").unwrap();
        assert_eq!(
            concept_graph(&arcs(), &concepts),
            "src,dst,weight\none,two,1\ntwo,one,2\n"
        );
    }

    #[test]
    fn placement_proposes_the_strongest_tie_and_lists_vanished_names() {
        let concepts = parse_concepts("## one: A\na/x.rs\n## two: B\na/y.rs\nold/w.rs\n").unwrap();
        let report = placement(&arcs(), &concepts);
        assert!(report.contains("not in the concept file: 1"), "{report}");
        assert!(report.contains("b/z.rs -> one (2)"), "{report}");
        assert!(report.contains("old/w.rs (in two)"), "{report}");
    }
}
