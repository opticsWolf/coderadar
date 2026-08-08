// CodeRadar v3.5 — Resolution: Orchestrator (§6.5-6.7)
// Five-layer resolution cascade, staged two-phase commit, ::toplevel sentinel.
//
// Cascade order (early exit on success):
//   L1: Stack Graphs      → confidence 0.90–1.00
//   L2: Import Graph      → confidence 0.80–0.89
//   L3: Signature Match   → confidence 0.40–0.79
//   L4: Embedding Fallback → confidence 0.20–0.39  (Python-side, deferred)
//   L5: LSP Override       → confidence 1.00        (Python-side, deferred)

use std::collections::HashMap;

use crate::resolve::cache::{ResolutionCache, Resolution};
use crate::types::SymbolId;
use crate::resolve::import_graph::{rank_candidates, resolve_in_imports};
use crate::resolve::signature::{signature_match, ScoredDef};
use crate::resolve::stack_graph::{self, ParsedReference, StackGraphResolver};
use crate::types::*;

/// Orchestrates the five-layer resolution cascade.
pub struct ResolutionOrchestrator {
    pub stack_graph: StackGraphResolver,
    pub cache: ResolutionCache,
    pub max_import_depth: usize,
    pub include_same_package: bool,
}

impl ResolutionOrchestrator {
    pub fn new() -> Self {
        Self {
            stack_graph: StackGraphResolver::new(),
            cache: ResolutionCache::new(),
            max_import_depth: 3,
            include_same_package: true,
        }
    }

    // ── Resolution Cascade (§6.1) ──────────────────────────────────────────

    /// Resolve a single reference through L1 → L2 → L3 fallthrough.
    ///
    /// Returns None when all layers fail (the reference stays `Unresolved`).
    /// The caller wraps this in the appropriate `ResolvedEdge` variant.
    pub fn resolve_reference(
        &mut self,
        file_path: &str,
        reference: &ParsedReference,
        import_graph: &crate::graph::ImportGraph,
        definitions_pool: &[ScoredDef],
        config: &crate::graph::SignatureConfig,
    ) -> Option<ResolvedEdge> {
        // ── L1: Stack Graphs ────────────────────────────────────────────
        if let Some(sg) = self.stack_graph.resolve_reference(file_path, reference) {
            return Some(ResolvedEdge {
                source_id: format!("{}::{}", file_path, reference.name),
                target_id: sg.target_name.clone(),
                confidence: sg.confidence,
                method: ResolutionMethod::StackGraph,
                provenance: EdgeProvenance::StackGraph,
                kind: reference.kind,
                line: sg.line as usize,
                call_site_span: ByteSpan { start: 0, end: 0 },
                args_span: None,
                target_kind: TargetKind::Internal,
            });
        }

        // ── L2: Import Graph + Scope ────────────────────────────────────
        if let Some((mut matches, l2_conf)) = resolve_in_imports(
            import_graph,
            file_path,
            &reference.name,
            self.max_import_depth,
            self.include_same_package,
        ) {
            rank_candidates(&mut matches, file_path);

            // Take the top-ranked candidate
            let best = &matches[0];
            let target_id = best
                .module_id
                .clone()
                .unwrap_or_else(|| format!("{}::{}", best.module_path, best.export_name));

            // Clamp confidence into L2 band [0.80, 0.89]
            let confidence = if matches.len() == 1 {
                0.89
            } else {
                (0.80_f32).max(l2_conf.min(0.89))
            };

            return Some(ResolvedEdge {
                source_id: format!("{}::{}", file_path, reference.name),
                target_id,
                confidence,
                method: ResolutionMethod::ImportConstrained,
                provenance: EdgeProvenance::ImportGraph,
                kind: reference.kind,
                line: reference.line as usize,
                call_site_span: ByteSpan { start: 0, end: 0 },
                args_span: None,
                target_kind: TargetKind::Internal,
            });
        }

        // ── L3: Signature Match ─────────────────────────────────────────
        if let Some(scored) = signature_match(
            &reference.name,
            None, // receiver context not available at this stage
            file_path,
            definitions_pool,
            config,
        ) {
            let best = &scored[0];
            let confidence = (0.40 + best.score * 0.39).clamp(0.40, 0.79);

            return Some(ResolvedEdge {
                source_id: format!("{}::{}", file_path, reference.name),
                target_id: best.entity_id.clone(),
                confidence,
                method: ResolutionMethod::SignatureMatch,
                provenance: EdgeProvenance::SignatureMatch,
                kind: reference.kind,
                line: reference.line as usize,
                call_site_span: ByteSpan { start: 0, end: 0 },
                args_span: None,
                target_kind: TargetKind::Internal,
            });
        }

        // ── Unresolved — L4/L5 defer to Python ──────────────────────────
        None
    }

    /// Run the full resolution cascade for all references in a file.
    pub fn resolve_file(
        &mut self,
        file_path: &str,
        references: &[ParsedReference],
        import_graph: &crate::graph::ImportGraph,
        definitions_pool: &[ScoredDef],
        config: &crate::graph::SignatureConfig,
    ) -> Vec<ResolvedEdge> {
        let mut edges = Vec::with_capacity(references.len());

        for reference in references {
            if let Some(edge) = self.resolve_reference(
                file_path,
                reference,
                import_graph,
                definitions_pool,
                config,
            ) {
                edges.push(edge);
            }
            // Unresolved references are silently recorded — the Python layer
            // may attempt L4/L5 and the agent queries can report them.
        }

        edges
    }

    /// Resolve calls for a function: classify call shape and resolve.
    pub fn resolve_calls(
        &self,
        calls: &[UnresolvedRef],
        function_id: &str,
        import_graph: &crate::graph::ImportGraph,
    ) -> Vec<ResolvedCall> {
        let mut results = Vec::with_capacity(calls.len());

        for call in calls {
            // Check cache first
            if let Some(resolution) = self.cache.get_import_target(function_id, &call.name) {
                match &resolution {
                    ImportResolution::Symbol(sym_id) => {
                        match sym_id {
                            SymbolId::Function(ent_id) | SymbolId::Module(ent_id) | SymbolId::Class(ent_id) | SymbolId::Import(ent_id) => {
                                results.push(ResolvedCall::Function(ent_id.clone()));
                                continue;
                            }
                        }
                    }
                    ImportResolution::Module(ent_id) => {
                        results.push(ResolvedCall::External(ent_id.clone()));
                        continue;
                    }
                    ImportResolution::Unresolved => {
                        // Fall through to resolution
                    }
                    ImportResolution::Wildcard { .. } | ImportResolution::Dynamic | ImportResolution::External { .. } => {
                        // Too ambiguous or external — fall through to resolution
                    }
                }
            }

            let resolved = self.resolve_single_call(call, import_graph);
            results.push(resolved);
        }

        results
    }

    /// Classify call shape and resolve based on path structure (§5.3.3).
    fn resolve_single_call(
        &self,
        call: &UnresolvedRef,
        import_graph: &crate::graph::ImportGraph,
    ) -> ResolvedCall {
        let name = &call.name;
        let path_len = call.path.len();

        match path_len {
            0 => {
                // Simple name: scope chain → imports → builtins
                self.resolve_simple_name(name, import_graph)
            }
            1 => {
                // self.method / cls.method / module.func / Class.method
                let prefix = &call.path[0];

                if prefix == "self" || prefix == "cls" || prefix == "this" {
                    // Method call on self/cls/this — deferred to MRO walk
                    ResolvedCall::Unresolved {
                        reason: UnresolvedReason::TypeInferenceRequired,
                        raw: call.clone(),
                    }
                } else if prefix.chars().next().map_or(false, |c| c.is_uppercase()) {
                    // Capitalized prefix → likely a class name
                    ResolvedCall::Method {
                        receiver: ReceiverShape::ClassRef(prefix.clone()),
                        method: format!("{}::{}", prefix, name),
                    }
                } else {
                    // Module prefix → try import-graph resolution
                    let full_name = format!("{}::{}", prefix, name);
                    if let Some((matches, _)) = resolve_in_imports(
                        import_graph,
                        prefix,
                        name,
                        3,
                        true,
                    ) {
                        if !matches.is_empty() {
                            let target = matches[0]
                                .module_id
                                .clone()
                                .unwrap_or_else(|| full_name.clone());
                            return ResolvedCall::Function(target);
                        }
                    }
                    // Not found — mark unresolved
                    ResolvedCall::Unresolved {
                        reason: UnresolvedReason::NameNotInScope,
                        raw: call.clone(),
                    }
                }
            }
            _ => {
                // module.submodule.Class.method → resolve left to right
                // For now, try the last segment as the name in the parent scope
                let parent = call.path.last().cloned().unwrap_or_default();
                let full_name = format!("{}::{}", parent, name);
                ResolvedCall::Unresolved {
                    reason: UnresolvedReason::TypeInferenceRequired,
                    raw: call.clone(),
                }
            }
        }
    }

    /// Resolve a simple (bare) name via import graph → cache → external fallback.
    fn resolve_simple_name(
        &self,
        name: &str,
        import_graph: &crate::graph::ImportGraph,
    ) -> ResolvedCall {
        // Builtins check — Python builtins
        if BUILTINS.contains(&name) {
            return ResolvedCall::Builtin(name.to_string());
        }

        // Check cache per module context
        // (In production, would check per-scope Module→Name→Resolution)

        // External fallback — we don't know where this lives, mark it
        // as External so the agent still sees the edge.
        ResolvedCall::External(name.to_string())
    }

    // ── C3 Linearization (§5.3.4) ───────────────────────────────────────────

    /// Compute MRO using C3 linearization.
    ///
    /// Formula: L[C] = C + merge(L[B1], L[B2], ..., [B1, B2, ...])
    ///
    /// Returns the linearized list and a boolean indicating whether the
    /// linearization is complete (false = external base encountered).
    pub fn c3_linearize(&self, class_id: &str, bases: &[EntityId], mro_cache: &HashMap<EntityId, Vec<MroNode>>) -> (Vec<MroNode>, bool) {
        let mut result: Vec<MroNode> = vec![MroNode::Class(class_id.to_string())];

        if bases.is_empty() {
            return (result, true);
        }

        // Collect linearizations of each base
        let mut base_lists: Vec<Vec<MroNode>> = Vec::new();

        for base in bases {
            if let Some(cached_mro) = mro_cache.get(base) {
                base_lists.push(cached_mro.clone());
            } else {
                // Unknown base — mark as external
                base_lists.push(vec![MroNode::External { name: base.clone() }]);
            }
        }

        // Add the base-class list itself (for merge ordering)
        base_lists.push(
            bases
                .iter()
                .map(|b| MroNode::Class(b.clone()))
                .collect(),
        );

        // C3 merge
        let mut complete = true;

        while base_lists.iter().any(|l| !l.is_empty()) {
            // Find a good head: one not in the tail of any other list
            let mut found = false;

            for i in 0..base_lists.len() {
                if base_lists[i].is_empty() {
                    continue;
                }

                let head = base_lists[i][0].clone();

                // Check: is head in the tail (position 1..) of any other list?
                let in_tail = base_lists
                    .iter()
                    .enumerate()
                    .any(|(j, list)| i != j && list.len() > 1 && list[1..].contains(&head));

                if !in_tail {
                    // Good head — add to result and remove from all lists
                    result.push(head.clone());
                    for list in &mut base_lists {
                        if list.first() == Some(&head) {
                            list.remove(0);
                        }
                    }
                    found = true;
                    break;
                }
            }

            if !found {
                // Inconsistent MRO — this means there's a diamond with
                // external bases in an unresolvable order.
                // Mark the remainder as unresolved.
                for list in &base_lists {
                    for node in list {
                        if matches!(node, MroNode::External { .. }) {
                            result.push(node.clone());
                            complete = false;
                        } else {
                            // Unresolvable internal dead-end
                            result.push(MroNode::External { name: "<unresolvable>".into() });
                        }
                    }
                }
                break;
            }
        }

        (result, complete)
    }

    /// Invalidate caches for a modified module.
    pub fn invalidate_module(&mut self, module_id: &str) {
        self.cache.invalidate_module(module_id);
    }

    /// Invalidate MRO caches for a class hierarchy.
    pub fn invalidate_class_hierarchy(&mut self, class_id: &str, subclasses: &[EntityId], max_depth: usize) {
        self.cache.invalidate_class_hierarchy(class_id, subclasses, max_depth);
    }
}

// ── Python Builtins ─────────────────────────────────────────────────────────

/// Standard Python builtins that resolve trivially.
static BUILTINS: &[&str] = &[
    "print", "len", "range", "int", "str", "float", "bool", "list", "dict",
    "set", "tuple", "type", "isinstance", "issubclass", "hasattr", "getattr",
    "setattr", "delattr", "super", "object", "property", "staticmethod",
    "classmethod", "abs", "all", "any", "bin", "chr", "dir", "divmod",
    "enumerate", "eval", "exec", "filter", "format", "frozenset", "globals",
    "hex", "id", "input", "iter", "locals", "map", "max", "min", "next",
    "oct", "open", "ord", "pow", "repr", "reversed", "round", "slice",
    "sorted", "sum", "vars", "zip", "Exception", "ValueError", "TypeError",
    "KeyError", "IndexError", "AttributeError", "RuntimeError", "ImportError",
    "OSError", "FileNotFoundError", "StopIteration", "NotImplementedError",
    "__import__", "__name__", "__file__", "__doc__", "__builtins__",
    "True", "False", "None",
];

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_c3_linearization_single_no_bases() {
        let orchestrator = ResolutionOrchestrator::new();
        let cache = HashMap::new();
        let (mro, complete) = orchestrator.c3_linearize("A", &[], &cache);
        assert!(complete);
        assert_eq!(mro.len(), 1);
        assert!(matches!(&mro[0], MroNode::Class(id) if id == "A"));
    }

    #[test]
    fn test_c3_linearization_single_base() {
        let mut cache = HashMap::new();
        cache.insert("B".to_string(), vec![MroNode::Class("B".into()), MroNode::Class("C".into())]);

        let orchestrator = ResolutionOrchestrator::new();
        let (mro, complete) = orchestrator.c3_linearize("A", &["B".into()], &cache);
        assert!(complete);
        assert_eq!(mro[0], MroNode::Class("A".into()));
        assert_eq!(mro[1], MroNode::Class("B".into()));
    }

    #[test]
    fn test_c3_linearization_diamond() {
        //  A
        // / \
        // B  C
        // \ /
        //  D
        let mut cache = HashMap::new();
        // B's MRO: B → A
        cache.insert("B".into(), vec![MroNode::Class("B".into()), MroNode::Class("A".into())]);
        // C's MRO: C → A
        cache.insert("C".into(), vec![MroNode::Class("C".into()), MroNode::Class("A".into())]);

        let orchestrator = ResolutionOrchestrator::new();
        let (mro, complete) = orchestrator.c3_linearize("D", &["B".into(), "C".into()], &cache);
        assert!(complete);
        // D → B → C → A
        assert_eq!(mro[0], MroNode::Class("D".into()));
        assert_eq!(mro[1], MroNode::Class("B".into()));
        assert_eq!(mro[2], MroNode::Class("C".into()));
        assert_eq!(mro[3], MroNode::Class("A".into()));
    }

    #[test]
    fn test_resolve_bare_builtin() {
        let orchestrator = ResolutionOrchestrator::new();
        let import_graph = crate::graph::ImportGraph::new();
        let call = UnresolvedRef {
            name: "print".into(),
            path: vec![],
            line: 1,
            col: 0,
        };
        let result = orchestrator.resolve_single_call(&call, &import_graph);
        assert!(matches!(result, ResolvedCall::Builtin(s) if s == "print"));
    }

    #[test]
    fn test_resolve_bare_unknown() {
        let orchestrator = ResolutionOrchestrator::new();
        let import_graph = crate::graph::ImportGraph::new();
        let call = UnresolvedRef {
            name: "unknown_secret_sauce".into(),
            path: vec![],
            line: 42,
            col: 4,
        };
        let result = orchestrator.resolve_single_call(&call, &import_graph);
        assert!(matches!(result, ResolvedCall::External(s) if s == "unknown_secret_sauce"));
    }
}
