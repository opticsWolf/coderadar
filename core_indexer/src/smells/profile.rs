// CodeRadar Stage 0.4 — strictness profiles for the smell engine.
//
//! A closed enum of threshold profiles evaluated INSIDE the rules (not
//! post-filtered on severity): `strict` catches more by lowering thresholds,
//! `loose` only flags egregious cases. fossil-mcp validates the demand
//! (`min_confidence` params, `config/presets.rs`) but its free-form presets
//! are also the cautionary tale — this stays a closed set so each level can
//! be pinned by goldens and monotonicity holds:
//! findings(Strict) ⊇ findings(Normal) ⊇ findings(Loose).

/// Sensitivity profile for an analysis run. `Normal` reproduces the
/// historical hardcoded numbers exactly (factor 1.0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Strictness {
    /// Thresholds drop ~40% — CI gates, careful audits.
    Strict,
    /// The current baseline behavior, unchanged.
    #[default]
    Normal,
    /// Thresholds rise ~80% — triage over legacy code, high-precision calls.
    Loose,
}

impl Strictness {
    /// Multiplier applied to each rule's baseline thresholds. Strict lowers
    /// them (catches more); Loose raises them.
    pub fn factor(self) -> f64 {
        match self {
            Strictness::Strict => 0.6,
            Strictness::Normal => 1.0,
            Strictness::Loose => 1.8,
        }
    }

    /// Scale a count-based threshold, keeping it meaningful (≥ 1).
    pub fn scale(self, base: usize) -> usize {
        ((base as f64 * self.factor()).round() as usize).max(1)
    }

    /// Confidence floor for derived analyses (dead code, clones). Strictness
    /// maps onto the confidence axis rather than inventing a second one:
    /// strict includes Low-tier findings, loose demands High+ only.
    pub fn confidence_floor(self) -> f32 {
        match self {
            Strictness::Strict => 0.40,
            Strictness::Normal => 0.60,
            Strictness::Loose => 0.80,
        }
    }

    /// Parse the MCP string parameter. Unknown values are a loud typed error,
    /// never silently coerced to Normal (honest-refusal principle).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "strict" => Ok(Strictness::Strict),
            "normal" | "" => Ok(Strictness::Normal),
            "loose" => Ok(Strictness::Loose),
            other => Err(format!(
                "unknown strictness '{other}' (expected: strict | normal | loose)"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Strictness::Strict => "strict",
            Strictness::Normal => "normal",
            Strictness::Loose => "loose",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factors_are_ordered_and_normal_is_identity() {
        assert!(Strictness::Strict.factor() < Strictness::Normal.factor());
        assert!(Strictness::Normal.factor() < Strictness::Loose.factor());
        assert_eq!(Strictness::default(), Strictness::Normal);
        assert_eq!(Strictness::Normal.factor(), 1.0);
    }

    #[test]
    fn scale_keeps_thresholds_meaningful() {
        assert_eq!(Strictness::Normal.scale(50), 50);
        assert_eq!(Strictness::Strict.scale(50), 30); // 0.6
        assert_eq!(Strictness::Loose.scale(50), 90); // 1.8
        // Small thresholds never collapse to 0.
        assert_eq!(Strictness::Strict.scale(1), 1);
        assert_eq!(Strictness::Normal.scale(2), 2);
    }

    #[test]
    fn confidence_floors_map_onto_tiers() {
        assert_eq!(Strictness::Strict.confidence_floor(), 0.40);
        assert_eq!(Strictness::Normal.confidence_floor(), 0.60);
        assert_eq!(Strictness::Loose.confidence_floor(), 0.80);
        assert!(
            Strictness::Strict.confidence_floor() < Strictness::Normal.confidence_floor(),
        );
    }

    #[test]
    fn parse_is_loud_on_unknown_values() {
        assert_eq!(Strictness::parse("strict").unwrap(), Strictness::Strict);
        assert_eq!(Strictness::parse("normal").unwrap(), Strictness::Normal);
        assert_eq!(Strictness::parse("").unwrap(), Strictness::Normal);
        assert_eq!(Strictness::parse("loose").unwrap(), Strictness::Loose);
        assert!(Strictness::parse("STRICT").is_err(), "case-sensitive: no silent coercion");
        assert!(Strictness::parse("very-strict").is_err());
        let err = Strictness::parse("maximal").unwrap_err();
        assert!(err.contains("maximal") && err.contains("strict | normal | loose"));
    }
}
