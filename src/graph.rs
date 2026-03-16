/// Dependency graph and PageRank with petgraph.
use petgraph::algo::page_rank;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::HashMap;

#[derive(Clone, Debug)]
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

/// Personalized PageRank for query-biased ranking (MCP use).
pub fn personalized_pagerank(
    graph: &DiGraph<String, f64>,
    seed_nodes: &[NodeIndex],
    damping: f64,
    iterations: usize,
) -> Vec<f64> {
    let n = graph.node_count();
    if n == 0 {
        return vec![];
    }

    // Build personalization vector
    let mut personalization = vec![0.0; n];
    if seed_nodes.is_empty() {
        personalization.iter_mut().for_each(|p| *p = 1.0 / n as f64);
    } else {
        let weight = 1.0 / seed_nodes.len() as f64;
        for &node in seed_nodes {
            personalization[node.index()] = weight;
        }
    }

    let mut ranks = vec![1.0 / n as f64; n];
    let mut new_ranks = vec![0.0; n];

    for _ in 0..iterations {
        for (i, p) in personalization.iter().enumerate() {
            new_ranks[i] = (1.0 - damping) * p;
        }
        for node in graph.node_indices() {
            let out_edges: Vec<_> = graph.edges(node).collect();
            let total_weight: f64 = out_edges.iter().map(|e| *e.weight()).sum();
            if total_weight > 0.0 {
                for edge in &out_edges {
                    let share = ranks[node.index()] * edge.weight() / total_weight;
                    new_ranks[edge.target().index()] += damping * share;
                }
            } else {
                // Dangling node: distribute to all via personalization
                for (i, p) in personalization.iter().enumerate() {
                    new_ranks[i] += damping * ranks[node.index()] * p;
                }
            }
        }
        std::mem::swap(&mut ranks, &mut new_ranks);
    }
    ranks
}
