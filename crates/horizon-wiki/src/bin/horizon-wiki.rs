//! The mechanical steps of the `project-wiki` skill:
//!
//! - `horizon-wiki store-dir PROJECT_ROOT` prints the project's wiki store.
//! - `horizon-wiki place EDGES.csv CONCEPTS.md` lists namespaces the concept
//!   file lacks (with the concept each is most tied to) and names it has that
//!   the graph no longer does.
//! - `horizon-wiki concept-graph EDGES.csv CONCEPTS.md` prints the directed
//!   weights between concepts.
//!
//! EDGES.csv is `code-db query REPO namespace_graph FORMAT=csv`.

use std::process::ExitCode;

use horizon_wiki::concepts::{concept_graph, parse_concepts, parse_namespace_arcs, placement};
use horizon_wiki::Store;

const USAGE: &str = "usage:
  horizon-wiki store-dir PROJECT_ROOT
  horizon-wiki place EDGES.csv CONCEPTS.md
  horizon-wiki concept-graph EDGES.csv CONCEPTS.md";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("horizon-wiki: {message}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn read(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

fn run(args: &[String]) -> Result<String, String> {
    match args {
        [command, root] if command == "store-dir" => {
            let store = Store::for_project(std::path::Path::new(root))
                .ok_or("no data home: set XDG_DATA_HOME or HOME")?;
            Ok(format!("{}\n", store.dir().display()))
        }
        [command, edges, concepts] if command == "place" || command == "concept-graph" => {
            let arcs = parse_namespace_arcs(&read(edges)?).map_err(|e| e.to_string())?;
            let concepts = parse_concepts(&read(concepts)?).map_err(|e| e.to_string())?;
            Ok(if command == "place" {
                placement(&arcs, &concepts)
            } else {
                concept_graph(&arcs, &concepts)
            })
        }
        _ => Err("unknown command or wrong arguments".into()),
    }
}
