//! The namespace graph as the clustering input: `namespace_graph`'s rows
//! (duckdb-ext/sql/queries/namespace_graph.sql) folded into an undirected,
//! weighted edge list.

use std::collections::BTreeMap;

/// Undirected and weighted; every edge is stored once with `u < v`.
#[derive(Debug, Clone, PartialEq)]
pub struct NamespaceGraph {
    /// Node index → namespace (a file or a `mod` block inside one).
    pub names: Vec<String>,
    /// Node index → SCIP package (the crate).
    pub packages: Vec<String>,
    /// `(u, v, weight)` with `u < v`, sorted.
    pub edges: Vec<(usize, usize, f64)>,
}

impl NamespaceGraph {
    pub fn node_count(&self) -> usize {
        self.names.len()
    }
}

const COLUMNS: [&str; 5] = [
    "src_module",
    "dst_module",
    "src_package",
    "dst_package",
    "unit_edges",
];

/// Reads `namespace_graph`'s CSV. An edge's weight is its unit edges that
/// have no test unit at either end (`unit_edges - test_unit_edges`); an edge
/// with none is dropped, and so is a namespace left with no edge. The two
/// directions of a pair are summed: the clustering asks how strongly two
/// namespaces are tied, not which one depends on the other.
pub fn parse_namespace_csv(text: &str) -> Result<NamespaceGraph, String> {
    let mut index: BTreeMap<String, String> = BTreeMap::new();
    let mut pairs: BTreeMap<(String, String), f64> = BTreeMap::new();
    for row in non_test_rows(text)? {
        index.insert(row.src.clone(), row.src_package);
        index.insert(row.dst.clone(), row.dst_package);
        let key = if row.src < row.dst {
            (row.src, row.dst)
        } else {
            (row.dst, row.src)
        };
        *pairs.entry(key).or_default() += row.weight;
    }

    let names: Vec<String> = index.keys().cloned().collect();
    let packages: Vec<String> = index.values().cloned().collect();
    let position = |name: &String| names.binary_search(name).expect("indexed above");
    let edges = pairs
        .iter()
        .map(|((a, b), w)| (position(a), position(b), *w))
        .collect();
    Ok(NamespaceGraph {
        names,
        packages,
        edges,
    })
}

/// The same CSV kept directed: `(src, dst, weight)` per ordered pair, with the
/// same non-test weight. A concept page states who depends on whom, which
/// the undirected graph has folded away.
pub fn parse_namespace_arcs(text: &str) -> Result<Vec<(String, String, f64)>, String> {
    let mut arcs: BTreeMap<(String, String), f64> = BTreeMap::new();
    for row in non_test_rows(text)? {
        *arcs.entry((row.src, row.dst)).or_default() += row.weight;
    }
    Ok(arcs.into_iter().map(|((a, b), w)| (a, b, w)).collect())
}

struct Row {
    src: String,
    dst: String,
    src_package: String,
    dst_package: String,
    weight: f64,
}

/// The rows with a non-test weight above zero.
fn non_test_rows(text: &str) -> Result<Vec<Row>, String> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header: Vec<&str> = lines
        .next()
        .ok_or("empty input: expected namespace_graph's CSV header")?
        .split(',')
        .collect();
    let column = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .ok_or_else(|| format!("missing column `{name}`"))
    };
    let [src, dst, src_package, dst_package, unit_edges] = COLUMNS.map(column);
    let (src, dst, src_package, dst_package, unit_edges) =
        (src?, dst?, src_package?, dst_package?, unit_edges?);
    let test_unit_edges = column("test_unit_edges")?;

    let mut rows = Vec::new();
    for (row, line) in lines.enumerate() {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() != header.len() {
            return Err(format!(
                "row {}: {} fields, header has {}",
                row + 2,
                fields.len(),
                header.len()
            ));
        }
        let number = |at: usize| {
            fields[at]
                .parse::<u64>()
                .map_err(|e| format!("row {}: `{}`: {e}", row + 2, fields[at]))
        };
        let weight = number(unit_edges)?.saturating_sub(number(test_unit_edges)?);
        if weight == 0 {
            continue;
        }
        rows.push(Row {
            src: fields[src].to_string(),
            dst: fields[dst].to_string(),
            src_package: fields[src_package].to_string(),
            dst_package: fields[dst_package].to_string(),
            weight: weight as f64,
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "src_module,dst_module,src_package,dst_package,unit_edges,occurrences,test_unit_edges,ambiguous_unit_edges,nested_modules,reverse_unit_edges";

    #[test]
    fn both_directions_sum_and_test_only_edges_drop() {
        let csv = format!(
            "{HEADER}\n\
             a.rs,b.rs,p,p,3,9,1,0,false,2\n\
             b.rs,a.rs,p,p,2,4,0,0,false,3\n\
             a.rs,tests.rs,p,p,5,5,5,0,false,0\n"
        );
        let graph = parse_namespace_csv(&csv).unwrap();
        assert_eq!(graph.names, ["a.rs", "b.rs"]);
        assert_eq!(graph.packages, ["p", "p"]);
        // 3 - 1 test edge, plus the reverse direction's 2.
        assert_eq!(graph.edges, [(0, 1, 4.0)]);
    }

    #[test]
    fn arcs_keep_each_direction_apart() {
        let csv = format!(
            "{HEADER}\n\
             a.rs,b.rs,p,p,3,9,1,0,false,2\n\
             b.rs,a.rs,p,p,2,4,0,0,false,3\n"
        );
        let arcs = parse_namespace_arcs(&csv).unwrap();
        assert_eq!(
            arcs,
            [
                ("a.rs".to_string(), "b.rs".to_string(), 2.0),
                ("b.rs".to_string(), "a.rs".to_string(), 2.0)
            ]
        );
    }

    #[test]
    fn a_missing_column_is_named() {
        let error = parse_namespace_csv("src_module,dst_module\n").unwrap_err();
        assert!(error.contains("src_package"), "{error}");
    }

    #[test]
    fn a_short_row_is_rejected_with_its_line() {
        let csv = format!("{HEADER}\na.rs,b.rs\n");
        let error = parse_namespace_csv(&csv).unwrap_err();
        assert!(error.starts_with("row 2"), "{error}");
    }
}
