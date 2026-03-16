/// Dependency graph and PageRank with petgraph.
use petgraph::algo::page_rank;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub enum EdgeKind {
    Import,
    ReExport,
    TypeImport,
}

pub struct DependencyGraph {
    pub graph: DiGraph<String, EdgeKind>,
    pub path_to_node: HashMap<String, NodeIndex>,
}

impl Default for DependencyGraph {
    fn default() -> Self {
        Self {
            graph: DiGraph::new(),
            path_to_node: HashMap::new(),
        }
    }
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&mut self, path: &str) -> NodeIndex {
        *self
            .path_to_node
            .entry(path.to_string())
            .or_insert_with(|| self.graph.add_node(path.to_string()))
    }

    pub fn add_edge(&mut self, from: &str, to: &str, kind: EdgeKind) {
        let from_idx = self.add_file(from);
        let to_idx = self.add_file(to);
        self.graph.add_edge(from_idx, to_idx, kind);
    }

    /// Standard PageRank — damping 0.85, 100 iterations
    pub fn compute_ranks(&self) -> Vec<(String, f64)> {
        let scores = page_rank(&self.graph, 0.85_f64, 100);
        let mut ranked: Vec<(String, f64)> = self
            .graph
            .node_indices()
            .map(|idx| (self.graph[idx].clone(), scores[idx.index()]))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }
}
