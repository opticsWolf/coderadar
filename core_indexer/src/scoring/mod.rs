// CodeRadar Stage 0.1 — shared confidence scoring for derived analyses.
//
//! Adapted conceptually from fossil-mcp `src/core/scoring.rs` (MIT OR
//! Apache-2.0); re-derived for CodeRadar's f32-confidence edge model and the
//! Tier vocabulary used by dead-code (Stage 1) and clones (Stage 2).
//!
//! Table-driven, pure, allocation-free: every later stage maps its raw
//! evidence through these helpers so confidence semantics stay consistent
//! across tools.

/// Confidence tier for a derived finding. Tiers gate visibility: MCP tools
/// hide `Speculative` behind explicit opt-in (`strictness` / `min_confidence`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// 0.0–0.4 : report only behind a flag.
    #[default]
    Speculative,
    /// 0.4–0.6 : weak evidence; include at `strict`.
    Low,
    /// 0.6–0.8 : actionable with a caveat; the default floor.
    Medium,
    /// 0.8–0.95 : strong evidence.
    High,
    /// 0.95+ : e.g. exact Type-1 clone, or zero incoming edges + entry-point proof.
    Certain,
}

impl Tier {
    /// Inclusive lower bound of the tier's score band.
    pub fn floor(self) -> f32 {
        match self {
            Tier::Speculative => 0.0,
            Tier::Low => 0.40,
            Tier::Medium => 0.60,
            Tier::High => 0.80,
            Tier::Certain => 0.95,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Speculative => "Speculative",
            Tier::Low => "Low",
            Tier::Medium => "Medium",
            Tier::High => "High",
            Tier::Certain => "Certain",
        }
    }
}

/// Combine independent evidence multiplicatively (naive-Bayes style).
///
/// Each input must be in (0, 1]; callers clamp before calling — an input of
/// 0.0 means "impossible" and annihilates the combination, which is rarely
/// what a heuristic wants to say.
pub fn combine(evidence: &[f32]) -> f32 {
    evidence.iter().product::<f32>().clamp(0.0, 1.0)
}

/// Map a score to its tier.
pub fn tier_of(score: f32) -> Tier {
    match score {
        s if s >= 0.95 => Tier::Certain,
        s if s >= 0.80 => Tier::High,
        s if s >= 0.60 => Tier::Medium,
        s if s >= 0.40 => Tier::Low,
        _ => Tier::Speculative,
    }
}

/// Clone confidence: similarity band × size factor.
///
/// Longer clones are less likely to be coincidental; very short ones are
/// down-weighted because a handful of similar lines occur by accident all
/// the time. Mirrors fossil-mcp `clone_confidence` bands, clamped to 1.0.
pub fn clone_confidence(similarity: f64, lines: usize) -> f32 {
    let base: f64 = if similarity > 0.95 {
        1.0
    } else if similarity > 0.80 {
        0.8
    } else if similarity > 0.60 {
        0.6
    } else {
        0.4
    };
    let size_factor: f64 = if lines > 50 {
        1.1
    } else if lines > 20 {
        1.0
    } else if lines > 10 {
        0.9
    } else {
        0.7
    };
    ((base * size_factor).min(1.0)) as f32
}

/// Dead-code confidence from reachability evidence.
///
/// `incoming_edges`: number of resolved incoming call edges in the current
/// projection; `is_entry_point`: whether the entity itself was detected as a
/// production entry point. Zero edges + non-entry is the Certain case; each
/// surviving edge drops confidence hard because dynamic dispatch may still
/// hide callers the graph did not resolve.
pub fn dead_code_confidence(incoming_edges: usize, is_entry_point: bool) -> f32 {
    if is_entry_point {
        return 0.05; // live by definition — reported only as an explanation
    }
    match incoming_edges {
        0 => 0.96,
        1 => 0.70,
        2 => 0.55,
        _ => 0.35,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_boundaries_are_pinned() {
        assert_eq!(tier_of(0.0), Tier::Speculative);
        assert_eq!(tier_of(0.399), Tier::Speculative);
        assert_eq!(tier_of(0.40), Tier::Low);
        assert_eq!(tier_of(0.599), Tier::Low);
        assert_eq!(tier_of(0.60), Tier::Medium);
        assert_eq!(tier_of(0.799), Tier::Medium);
        assert_eq!(tier_of(0.80), Tier::High);
        assert_eq!(tier_of(0.949), Tier::High);
        assert_eq!(tier_of(0.95), Tier::Certain);
        assert_eq!(tier_of(1.0), Tier::Certain);
    }

    #[test]
    fn combine_is_multiplicative_and_clamped() {
        let eps = 1e-6;
        assert!((combine(&[0.9, 0.9]) - 0.81).abs() < eps);
        assert!((combine(&[]) - 1.0).abs() < eps, "empty product is identity");
        assert!(combine(&[1.5, 1.5]) <= 1.0, "clamped to 1.0");
        assert!(combine(&[0.0, 0.9]) == 0.0);
    }

    #[test]
    fn clone_confidence_bands_and_size_factor() {
        // Band boundaries (> not >=, per the table).
        let long = clone_confidence(0.97, 60); // base 1.0 × 1.1 → clamp
        assert_eq!(long, 1.0);

        let mid = clone_confidence(0.85, 30); // 0.8 × 1.0
        assert!((mid - 0.8).abs() < 1e-6);

        let short_weak = clone_confidence(0.50, 5); // 0.4 × 0.7
        assert!((short_weak - 0.28).abs() < 1e-4);
        assert_eq!(tier_of(short_weak), Tier::Speculative);

        let medium_short = clone_confidence(0.96, 12); // 1.0 × 0.9
        assert!((medium_short - 0.9).abs() < 1e-6);
        let band_edge = clone_confidence(0.90, 12); // 0.8 × 0.9 — just under the top band
        assert!((band_edge - 0.72).abs() < 1e-6);
        assert_eq!(tier_of(band_edge), Tier::Medium);

        let tiny_exact = clone_confidence(0.99, 3); // 1.0 × 0.7
        assert!((tiny_exact - 0.7).abs() < 1e-6, "tiny clones never look Certain");
    }

    #[test]
    fn dead_code_confidence_matches_evidence() {
        assert_eq!(dead_code_confidence(0, false), 0.96);
        assert_eq!(tier_of(dead_code_confidence(0, false)), Tier::Certain);
        assert_eq!(dead_code_confidence(0, true), 0.05, "entry points are live");
        assert_eq!(tier_of(dead_code_confidence(1, false)), Tier::Medium);
        assert_eq!(tier_of(dead_code_confidence(5, false)), Tier::Speculative);
    }

    #[test]
    fn tiers_order_from_speculative_to_certain() {
        assert!(Tier::Speculative < Tier::Low);
        assert!(Tier::Low < Tier::Medium);
        assert!(Tier::Medium < Tier::High);
        assert!(Tier::High < Tier::Certain);
    }
}
