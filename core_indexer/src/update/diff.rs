// CodeRadar v3.6 — Incremental Update: Tiered Diff Algorithm (§5.2)
// Matches entities by identity, not position. Produces Add | Remove | Modify patches.

use crate::types::*;

impl ExtractedUnit {
    /// Get the entity ID for this unit.
    pub fn entity_id(&self) -> EntityId {
        match self {
            ExtractedUnit::Module(m) => m.id.clone(),
            ExtractedUnit::Class(c) => c.id.clone(),
            ExtractedUnit::Function(f) => f.id.clone(),
            ExtractedUnit::Import(i) => i.id.clone(),
            ExtractedUnit::Constant(c) => c.id.clone(),
            ExtractedUnit::TypeAlias(t) => t.id.clone(),
            ExtractedUnit::Field(f) => f.name.clone(),
        }
    }
}

/// A single diff operation for one entity.
#[derive(Clone, Debug)]
pub enum DiffOp {
    Insert {
        unit: ExtractedUnit,
    },
    Remove {
        kind: EntityKind,
        old_id: Option<EntityId>,
    },
    Modify {
        kind: EntityKind,
        id: EntityId,
        signature_changed: bool,
        body_changed: bool,
        new_unit: ExtractedUnit,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntityKind {
    Module,
    Class,
    Function,
    Import,
    Constant,
    TypeAlias,
}

/// Tiered match key for diff (§5.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MatchKey {
    kind: EntityKind,
    qualified_name: String,
    signature_hash: Option<u64>,
    body_hash: Option<u64>,
}

impl ExtractedUnit {
    /// Produce a MatchKey for diff comparison.
    pub fn match_key(&self) -> MatchKey {
        match self {
            ExtractedUnit::Module(m) => MatchKey {
                kind: EntityKind::Module,
                qualified_name: m.name.clone(),
                signature_hash: None,
                body_hash: None,
            },
            ExtractedUnit::Class(c) => MatchKey {
                kind: EntityKind::Class,
                qualified_name: c.qualified_name.clone(),
                signature_hash: None,
                body_hash: None,
            },
            ExtractedUnit::Function(f) => MatchKey {
                kind: EntityKind::Function,
                qualified_name: f.qualified_name.clone(),
                signature_hash: Some(f.signature_hash),
                body_hash: Some(f.body_hash),
            },
            ExtractedUnit::Import(i) => MatchKey {
                kind: EntityKind::Import,
                qualified_name: i.raw.clone(),
                signature_hash: None,
                body_hash: None,
            },
            ExtractedUnit::Field(f) => MatchKey {
                kind: EntityKind::Import, // Fields not independently keyed
                qualified_name: f.name.clone(),
                signature_hash: None,
                body_hash: None,
            },
            ExtractedUnit::Constant(c) => MatchKey {
                kind: EntityKind::Constant,
                qualified_name: c.name.clone(),
                signature_hash: None,
                body_hash: None,
            },
            ExtractedUnit::TypeAlias(t) => MatchKey {
                kind: EntityKind::TypeAlias,
                qualified_name: t.name.clone(),
                signature_hash: None,
                body_hash: None,
            },
        }
    }
}

/// Diff two sets of ExtractedUnits (old vs new) and produce a patch.
pub fn diff_units(old_units: &[ExtractedUnit], new_units: &[ExtractedUnit]) -> Vec<DiffOp> {
    // Build lookup maps by qualified name
    let old_by_name: Vec<(&ExtractedUnit, MatchKey)> =
        old_units.iter().map(|u| (u, u.match_key())).collect();
    let new_by_name: Vec<(&ExtractedUnit, MatchKey)> =
        new_units.iter().map(|u| (u, u.match_key())).collect();

    let mut ops = Vec::new();

    // Step 1-3: Match by precedence
    // Tier 1: Exact match (kind + qname + sig_hash + body_hash identical) → no-op
    // Tier 2: Same identity, body changed
    // Tier 3: Same identity, signature changed

    let mut old_matched = vec![false; old_by_name.len()];
    let mut new_matched = vec![false; new_by_name.len()];

    for (new_idx, (new_unit, new_key)) in new_by_name.iter().enumerate() {
        for (old_idx, (_, old_key)) in old_by_name.iter().enumerate() {
            if old_matched[old_idx] {
                continue;
            }
            if new_key.kind != old_key.kind
                || new_key.qualified_name != old_key.qualified_name
            {
                continue;
            }

            // Match found — check tier
            let sig_changed = new_key.signature_hash != old_key.signature_hash;
            let body_changed = new_key.body_hash != old_key.body_hash;

            if !sig_changed && !body_changed {
                // Tier 1: identical, no-op
                old_matched[old_idx] = true;
                new_matched[new_idx] = true;
                break;
            }

            ops.push(DiffOp::Modify {
                kind: new_key.kind,
                id: new_unit.entity_id(),
                signature_changed: sig_changed,
                body_changed: body_changed && !sig_changed,
                new_unit: (*new_unit).clone(),
            });
            old_matched[old_idx] = true;
            new_matched[new_idx] = true;
            break;
        }
    }

    // Step 4: Unmatched old → Remove
    for (old_idx, (_, old_key)) in old_by_name.iter().enumerate() {
        if !old_matched[old_idx] {
            ops.push(DiffOp::Remove {
                kind: old_key.kind,
                old_id: Some(old_key.qualified_name.clone()),
            });
        }
    }

    // Step 5: Unmatched new → Insert
    for (new_idx, (new_unit, _)) in new_by_name.iter().enumerate() {
        if !new_matched[new_idx] {
            ops.push(DiffOp::Insert {
                unit: (*new_unit).clone(),
            });
        }
    }

    ops
}

/// Order operations within a patch for safe application (§5.2):
/// 1. Inserts (modules → classes → functions → imports)
/// 2. Modifications
/// 3. Removals (reverse order: imports → functions → classes → modules)
pub fn order_patch_ops(ops: &mut Vec<DiffOp>) {
    fn op_priority(op: &DiffOp) -> (u8, u8) {
        match op {
            DiffOp::Insert { unit } => {
                let phase = 0u8; // inserts first
                let kind_order = match unit {
                    ExtractedUnit::Module(_) => 0,
                    ExtractedUnit::Class(_) => 1,
                    ExtractedUnit::Function(_) => 2,
                    ExtractedUnit::Import(_) => 3,
                    ExtractedUnit::Constant(_) => 4,
                    ExtractedUnit::TypeAlias(_) => 5,
                    ExtractedUnit::Field(_) => 6,
                };
                (phase, kind_order)
            }
            DiffOp::Modify { .. } => (1, 0),
            DiffOp::Remove { kind, .. } => {
                let kind_order = match kind {
                    EntityKind::Import => 0,
                    EntityKind::Constant => 1,
                    EntityKind::TypeAlias => 2,
                    EntityKind::Function => 3,
                    EntityKind::Class => 4,
                    EntityKind::Module => 5,
                };
                (2, kind_order)
            }
        }
    }

    ops.sort_by_key(op_priority);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fn(name: &str, sig_hash: u64, body_hash: u64) -> ExtractedUnit {
        ExtractedUnit::Function(ExtractedFunction {
            id: format!("test.py::{name}"),
            name: name.into(),
            qualified_name: name.into(),
            parent_module: "test.py".into(),
            parent_class: None,
            parameters: vec![],
            return_type: None,
            calls: vec![],
            decorators: vec![],
            docstring: None,
            kind: FunctionKind::Free,
            is_async: false,
            is_generator: false,
            line: 1,
            exit_line: 2,
            source: SourceType::Impl,
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            signature_hash: sig_hash,
            body_hash,
            span: ByteSpan { start: 0, end: 10 },
            name_span: ByteSpan { start: 0, end: 5 },
            params_span: ByteSpan { start: 0, end: 0 },
            body_span: ByteSpan { start: 0, end: 0 },
            decorators_span: None,
        })
    }

    fn make_class(name: &str) -> ExtractedUnit {
        ExtractedUnit::Class(ExtractedClass {
            id: format!("test.py::{name}"),
            name: name.into(),
            qualified_name: name.into(),
            grammar_kind: "class_definition".into(),
            parent_module: "test.py".into(),
            parent_class: None,
            bases: vec![],
            decorators: vec![],
            docstring: None,
            fields: vec![],
            line: 1,
            exit_line: 2,
            source: SourceType::Impl,
            is_type_checking_only: false,
            parse_quality: ParseQuality::Clean,
            span: ByteSpan { start: 0, end: 10 },
            name_span: ByteSpan { start: 0, end: 5 },
            body_span: ByteSpan { start: 0, end: 0 },
            decorators_span: None,
        })
    }

    #[test]
    fn test_identical_units_produce_no_ops() {
        let old = vec![make_fn("foo", 100, 200)];
        let new = vec![make_fn("foo", 100, 200)];
        let ops = diff_units(&old, &new);
        assert!(ops.is_empty(), "Identical units should produce zero ops");
    }

    #[test]
    fn test_body_change_only_no_signature_change() {
        let old = vec![make_fn("foo", 100, 200)];
        let new = vec![make_fn("foo", 100, 300)]; // only body_hash changed
        let ops = diff_units(&old, &new);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::Modify { signature_changed, body_changed, .. } => {
                assert!(!*signature_changed);
                assert!(*body_changed);
            }
            _ => panic!("Expected Modify"),
        }
    }

    #[test]
    fn test_signature_change() {
        let old = vec![make_fn("foo", 100, 200)];
        let new = vec![make_fn("foo", 999, 200)]; // sig changed, body same
        let ops = diff_units(&old, &new);
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            DiffOp::Modify { signature_changed, .. } => {
                assert!(*signature_changed);
            }
            _ => panic!("Expected Modify"),
        }
    }

    #[test]
    fn test_new_entity_inserted() {
        let old: Vec<ExtractedUnit> = vec![];
        let new = vec![make_fn("bar", 1, 1)];
        let ops = diff_units(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], DiffOp::Insert { .. }));
    }

    #[test]
    fn test_old_entity_removed() {
        let old = vec![make_fn("baz", 1, 1)];
        let new: Vec<ExtractedUnit> = vec![];
        let ops = diff_units(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], DiffOp::Remove { .. }));
    }

    #[test]
    fn test_order_patch_inserts_before_modifies_before_removes() {
        let mut ops = vec![
            DiffOp::Remove { kind: EntityKind::Class, old_id: Some("cls1".into()) },
            DiffOp::Insert { unit: make_fn("newfn", 1, 1) },
            DiffOp::Modify { kind: EntityKind::Function, id: "fn1".into(), signature_changed: true, body_changed: false, new_unit: make_fn("foo", 2, 2) },
        ];
        order_patch_ops(&mut ops);
        // After ordering: Insert first, then Modify, then Remove
        assert!(matches!(ops[0], DiffOp::Insert { .. }));
        assert!(matches!(ops[1], DiffOp::Modify { .. }));
        assert!(matches!(ops[2], DiffOp::Remove { .. }));
    }

    #[test]
    fn test_diff_mixed_scenario() {
        let old = vec![
            make_fn("kept_same", 10, 20),
            make_fn("renamed", 30, 40),
        ];
        let new = vec![
            make_fn("kept_same", 10, 20),  // identical → no-op
            make_fn("renamed", 30, 99),     // body changed
            make_fn("added", 50, 60),        // new
        ];
        let ops = diff_units(&old, &new);
        // 2 ops: Modify for renamed, Insert for added
        assert_eq!(ops.len(), 2);
    }
}
