//! `cluster-spike run EDGES.csv --algo NAME [--resolution R] [--seed S]`
//! `cluster-spike sweep EDGES.csv [--resolutions R,..] [--cpm-resolutions R,..] [--seeds S,..]`
//! `cluster-spike concepts EDGES.csv CONCEPTS.md --algo NAME [--resolution R] [--seed S]`
//! `cluster-spike concept-graph EDGES.csv CONCEPTS.md`
//! `cluster-spike place EDGES.csv CONCEPTS.md`
//!
//! EDGES.csv is `code-db query REPO namespace_graph FORMAT=csv` (README.md).

use std::process::ExitCode;

use cluster_spike::algorithms::{Algorithm, run};
use cluster_spike::concepts::{assign, comparison, concept_graph, parse_concepts, placement};
use cluster_spike::graph::{parse_namespace_arcs, parse_namespace_csv};
use cluster_spike::metrics::rank_by_size;
use cluster_spike::report::{partition_listing, sweep_table};

const USAGE: &str = "usage:
  cluster-spike run EDGES.csv --algo NAME [--resolution R] [--seed S]
  cluster-spike sweep EDGES.csv [--resolutions R,..] [--cpm-resolutions R,..] [--seeds S,..]
  cluster-spike concepts EDGES.csv CONCEPTS.md --algo NAME [--resolution R] [--seed S]
  cluster-spike concept-graph EDGES.csv CONCEPTS.md
  cluster-spike place EDGES.csv CONCEPTS.md
algorithms: leiden, leiden-cpm, louvain-leiden-rs, louvain-graphrs";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("cluster-spike: {message}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(args: &[String]) -> Result<String, String> {
    let [command, path, options @ ..] = args else {
        return Err("expected a command and an edge file".to_string());
    };
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let graph = parse_namespace_csv(&text)?;
    let option = |name: &str| -> Result<Option<&str>, String> {
        match options.iter().position(|a| a == name) {
            None => Ok(None),
            Some(i) => options
                .get(i + 1)
                .map(|v| Some(v.as_str()))
                .ok_or_else(|| format!("{name} needs a value")),
        }
    };
    let numbers = |value: &str| -> Result<Vec<f64>, String> {
        value
            .split(',')
            .map(|v| v.parse::<f64>().map_err(|e| format!("`{v}`: {e}")))
            .collect()
    };
    let concept_file = || -> Result<Vec<cluster_spike::concepts::Concept>, String> {
        let file = options
            .first()
            .filter(|a| !a.starts_with("--"))
            .ok_or("expected a concept file after the edge file")?;
        let source = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
        parse_concepts(&source)
    };
    let partition_options = || -> Result<(Algorithm, f64, u64), String> {
        let name = option("--algo")?.ok_or("--algo is required")?;
        let algorithm =
            Algorithm::parse(name).ok_or_else(|| format!("unknown algorithm `{name}`"))?;
        let resolution = option("--resolution")?.map_or(Ok(1.0), |v| {
            v.parse().map_err(|e| format!("--resolution: {e}"))
        })?;
        let seed =
            option("--seed")?.map_or(Ok(1), |v| v.parse().map_err(|e| format!("--seed: {e}")))?;
        Ok((algorithm, resolution, seed))
    };
    match command.as_str() {
        "run" => {
            let (algorithm, resolution, seed) = partition_options()?;
            partition_listing(&graph, algorithm, resolution, seed)
        }
        "concept-graph" => {
            let concepts = concept_file()?;
            Ok(concept_graph(&parse_namespace_arcs(&text)?, &concepts))
        }
        "place" => {
            let concepts = concept_file()?;
            Ok(placement(&graph, &parse_namespace_arcs(&text)?, &concepts))
        }
        "concepts" => {
            let concepts = concept_file()?;
            let concept_of = assign(&graph, &concepts)?;
            let (algorithm, resolution, seed) = partition_options()?;
            // Numbered as `run` lists them, so "cluster N" matches its listing.
            let cluster_of = rank_by_size(&run(algorithm, &graph, resolution, seed)?);
            Ok(comparison(&graph, &concepts, &concept_of, &cluster_of))
        }
        "sweep" => {
            let resolutions = numbers(option("--resolutions")?.unwrap_or("0.5,1,2,4"))?;
            let cpm = numbers(option("--cpm-resolutions")?.unwrap_or("0.05,0.2,1,3"))?;
            let seeds: Vec<u64> = numbers(option("--seeds")?.unwrap_or("1,2,3,4,5"))?
                .into_iter()
                .map(|s| s as u64)
                .collect();
            let mut runs = Vec::new();
            for algorithm in Algorithm::ALL {
                let values = if algorithm.uses_cpm() {
                    &cpm
                } else {
                    &resolutions
                };
                runs.extend(values.iter().map(|&r| (algorithm, r)));
            }
            sweep_table(&graph, &runs, &seeds)
        }
        other => Err(format!("unknown command `{other}`")),
    }
}
