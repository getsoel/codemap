/// Keyword-based file scoring for context-aware file suggestions.
use crate::db;
use std::collections::HashMap;

const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "to", "for", "and", "or",
    "of", "in", "on", "with", "at", "by", "from", "as", "it", "its", "this", "that", "do", "does",
    "did", "has", "have", "had", "will", "would", "could", "should", "can", "may", "might",
    "shall", "not", "no", "but", "if", "then", "so", "than", "too", "very", "just", "about",
    "into", "over", "after", "before", "between", "through", "during", "each", "every", "all",
    "both", "some", "any", "most", "other", "new", "old",
];

pub struct ScoredFile {
    pub path: String,
    pub score: f64,
    pub rank: f64,
    pub match_reasons: Vec<String>,
}

/// Split query into lowercase keywords, removing stop words and duplicates.
pub fn tokenize_query(query: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| !w.is_empty())
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

pub fn score_files(
    keywords: &[String],
    files: &[db::FileWithExports],
    conn: &rusqlite::Connection,
) -> Vec<ScoredFile> {
    // Phase 1: direct keyword matching
    let mut scored: HashMap<String, ScoredFile> = HashMap::new();

    for file in files {
        let mut raw_score = 0.0;
        let mut reasons: Vec<String> = Vec::new();

        // Filename match: split path on / and . to get components
        let path_lower = file.path.to_lowercase();
        let path_components: Vec<&str> = path_lower
            .split(['/', '.'])
            .filter(|s| !s.is_empty())
            .collect();

        for kw in keywords {
            // Check if any path component contains the keyword
            if path_components
                .iter()
                .any(|comp| comp.contains(kw.as_str()))
            {
                raw_score += 3.0;
                if !reasons.contains(kw) {
                    reasons.push(kw.clone());
                }
            }
        }

        // Export name match
        for kw in keywords {
            if file
                .exports
                .iter()
                .any(|exp| exp.to_lowercase().contains(kw.as_str()))
            {
                raw_score += 2.0;
                if !reasons.contains(kw) {
                    reasons.push(kw.clone());
                }
            }
        }

        if raw_score > 0.0 {
            // Multiply by PageRank — use a floor so rank=0 files still appear
            let rank = file.rank.max(0.01);
            let final_score = raw_score * rank;
            scored.insert(
                file.path.clone(),
                ScoredFile {
                    path: file.path.clone(),
                    score: final_score,
                    rank: file.rank,
                    match_reasons: reasons,
                },
            );
        }
    }

    // Phase 2: graph expansion — 1-hop neighbors of directly scored files
    // Pre-build rank lookup for O(1) neighbor rank resolution
    let rank_map: HashMap<&str, f64> = files.iter().map(|f| (f.path.as_str(), f.rank)).collect();

    let direct_files: Vec<(String, f64)> = scored
        .iter()
        .map(|(path, sf)| (path.clone(), sf.score))
        .collect();

    for (path, parent_score) in &direct_files {
        let neighbor_score = parent_score * 0.3;

        for direction in &["imports", "importers"] {
            if let Ok(deps) = db::get_file_deps(conn, path, direction) {
                for dep in deps {
                    let neighbor_path = &dep.file_path;
                    if let Some(existing) = scored.get_mut(neighbor_path) {
                        if neighbor_score > existing.score {
                            existing.score = neighbor_score;
                        }
                    } else {
                        let rank = rank_map.get(neighbor_path.as_str()).copied().unwrap_or(0.0);
                        let reasons = vec![format!("neighbor of {}", path)];
                        scored.insert(
                            neighbor_path.clone(),
                            ScoredFile {
                                path: neighbor_path.clone(),
                                score: neighbor_score,
                                rank,
                                match_reasons: reasons,
                            },
                        );
                    }
                }
            }
        }
    }

    // Sort by score descending
    let mut result: Vec<ScoredFile> = scored.into_values().collect();
    result.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    result
}
