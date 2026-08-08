// CodeRadar v3.3 — Resolution: Signature Matching Layer 3 (§6.4)
// Language-agnostic name + arity matching biased toward proximity.

use crate::graph::SignatureConfig;

/// A scored definition candidate from Layer 3.
#[derive(Clone, Debug)]
pub struct ScoredDef {
    pub entity_id: String,
    pub name: String,
    pub arity: usize,
    pub file_path: String,
    pub score: f32,
}

/// Signature-match resolution when L1 and L2 have failed.
///
/// Weights default to name=0.4, arity=0.3, proximity=0.3.
/// Confidence clamped to [0.40, 0.79] — disjoint from L2 floor (0.80).
pub fn signature_match(
    name: &str,
    receiver: Option<&str>,
    file_path: &str,
    definitions: &[ScoredDef],
    config: &SignatureConfig,
) -> Option<Vec<ScoredDef>> {
    let mut scored: Vec<ScoredDef> = definitions
        .iter()
        .filter(|d| d.name == name)
        .map(|d| {
            let arity_score = arity_similarity(d.arity, receiver) * config.arity_weight;
            let name_score = name_exact_score(name, &d.name) * config.name_weight;
            let proximity_score =
                proximity_score_fn(file_path, &d.file_path) * config.proximity_weight;
            let total_score = arity_score + name_score + proximity_score;

            ScoredDef {
                entity_id: d.entity_id.clone(),
                name: d.name.clone(),
                arity: d.arity,
                file_path: d.file_path.clone(),
                score: total_score,
            }
        })
        .filter(|s| s.score >= config.min_score)
        .collect();

    if scored.is_empty() {
        return None;
    }

    // Sort by score descending
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Clamp final confidence: (0.40 + score * 0.39).clamp(0.40, 0.79)
    // but raw scores are returned; the orchestrator clamps.
    Some(scored)
}

/// Compute arity similarity based on receiver shape.
fn arity_similarity(arity: usize, _receiver: Option<&str>) -> f32 {
    // Simple: higher arity match = higher score
    if arity == 0 {
        0.5
    } else {
        0.8
    }
}

/// Exact name match = 1.0, partial = scaled by edit distance.
fn name_exact_score(query: &str, candidate: &str) -> f32 {
    if query == candidate {
        1.0
    } else {
        // Simple starts-with heuristic
        if candidate.starts_with(query) {
            0.8
        } else {
            0.5
        }
    }
}

/// Proximity score: deeper common prefix = higher score.
fn proximity_score_fn(query_file: &str, candidate_file: &str) -> f32 {
    let query_parts: Vec<&str> = query_file.split('/').collect();
    let cand_parts: Vec<&str> = candidate_file.split('/').collect();

    let common = query_parts
        .iter()
        .zip(cand_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();

    if common == 0 {
        0.1
    } else if common >= query_parts.len() - 1 && common >= cand_parts.len() - 1 {
        1.0 // same directory
    } else {
        0.3 + 0.3 * common as f32 / query_parts.len().max(1) as f32
    }
}
