// Projection diff/update tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;

/// Canonical id for an entity `update_file` wrote (F14): the write-time
/// canonical form, built the same way the write path builds it, so the
/// tests assert agreement of FORM — not a hardcoded spelling that would
/// re-pin whatever the implementation happens to mint today.
fn canon_id(file: &str, name: &str) -> String {
    format!(
        "{}::{}",
        crate::graph::module_resolution::canonical_file_form(file),
        name
    )
}

/// `insert_extracted` stored `name_span` in `params_span`. `build_fragment`
/// and `apply_diff_update` both got it right, so the divergence was invisible
/// outside the `index_file` path — until `plan_signature_update` replaced
/// `params_span` verbatim and overwrote the function *name* with the new
/// parameter list.
#[test]
fn test_index_file_records_params_span_not_name_span() {
    let source = "def greet(name, greeting=\"hi\"):\n    return greeting\n";
    let graph = CodeGraph::new(GraphConfig::default());
    graph
        .index_file(source, "spans.py", &Language::Python)
        .unwrap();

    let snap = graph.snapshot();
    let f = snap
        .functions
        .get("spans.py::greet")
        .expect("greet indexed");

    assert_ne!(
        f.params_span, f.name_span,
        "params_span must not alias name_span"
    );
    assert_eq!(&source[f.name_span.start..f.name_span.end], "greet");
    assert_eq!(
        &source[f.params_span.start..f.params_span.end],
        "(name, greeting=\"hi\")",
    );
}

/// All three ingest paths must agree on every span they record.
#[test]
fn test_index_file_and_update_file_agree_on_spans() {
    let source = "def greet(name, greeting=\"hi\"):\n    return greeting\n";

    let indexed = CodeGraph::new(GraphConfig::default());
    indexed
        .index_file(source, "spans.py", &Language::Python)
        .unwrap();
    let via_index = indexed
        .snapshot()
        .functions
        .get("spans.py::greet")
        .cloned()
        .unwrap();

    let updated = CodeGraph::new(GraphConfig::default());
    updated
        .index_file("def greet(): pass\n", "spans.py", &Language::Python)
        .unwrap();
    updated.update_file("spans.py", Some(source), None).unwrap();
    // F14: update_file mints the canonical form — same spans, one id form.
    let via_update = updated
        .snapshot()
        .functions
        .get(&canon_id("spans.py", "greet"))
        .cloned()
        .unwrap();

    assert_eq!(via_index.name_span, via_update.name_span);
    assert_eq!(via_index.params_span, via_update.params_span);
    assert_eq!(via_index.body_span, via_update.body_span);
    assert_eq!(via_index.parameters.len(), via_update.parameters.len());
}

/// A body edit that leaves the signature alone must still reach the graph.
#[test]
fn test_update_file_reindexes_a_changed_function() {
    let graph = CodeGraph::new(GraphConfig::default());
    // Seeded through update_file so the whole test lives in the
    // canonical id form (F14) — index_file mints bare test ids.
    graph
        .update_file("chg.py", Some("def f():\n    return 1\n"), None)
        .unwrap();
    let before = graph
        .snapshot()
        .functions
        .get(&canon_id("chg.py", "f"))
        .cloned()
        .unwrap();

    let outcome = graph
        .update_file("chg.py", Some("def f():\n    return 2\n"), None)
        .unwrap();
    let (added, removed) = (outcome.entities_added, outcome.entities_removed);
    // A changed entity is replaced in place: one insert, and no removal —
    // the removal counter tracks entities that disappeared from the file.
    assert_eq!((added, removed), (1, 0));

    let after = graph
        .snapshot()
        .functions
        .get(&canon_id("chg.py", "f"))
        .cloned()
        .unwrap();
    assert_ne!(
        before.body_hash, after.body_hash,
        "body_hash must track the body"
    );
    assert_eq!(
        before.signature_hash, after.signature_hash,
        "signature is unchanged"
    );
}

/// tree-sitter recovers from syntax errors instead of failing, so a broken
/// file still indexes. update_file used to report `clean` / 0 errors
/// regardless, which made every caller's failure branch unreachable.
#[test]
fn test_update_file_reports_a_recovered_parse() {
    let graph = CodeGraph::new(GraphConfig::default());
    graph
        .index_file("def f():\n    return 1\n", "broken.py", &Language::Python)
        .unwrap();

    let outcome = graph
        .update_file("broken.py", Some("def f(:\n    return 1\n"), None)
        .unwrap();

    assert_eq!(outcome.parse_quality, ParseQuality::Partial);
    assert!(
        outcome.parse_errors > 0,
        "a recovered parse has error nodes"
    );
}

#[test]
fn test_update_file_reports_a_clean_parse() {
    let graph = CodeGraph::new(GraphConfig::default());
    graph
        .index_file("def f():\n    return 1\n", "ok.py", &Language::Python)
        .unwrap();

    let outcome = graph
        .update_file("ok.py", Some("def f():\n    return 2\n"), None)
        .unwrap();

    assert_eq!(outcome.parse_quality, ParseQuality::Clean);
    assert_eq!(outcome.parse_errors, 0);
    assert!(outcome.elapsed_ms > 0.0, "elapsed_ms was hardcoded to 0.0");
}

/// The entity carries the quality of its own subtree, not the file's: a
/// syntax error in one function must not mark its neighbours Partial.
#[test]
fn test_parse_quality_is_recorded_per_entity() {
    let graph = CodeGraph::new(GraphConfig::default());
    graph
        .index_file(
            "def broken(:\n    return 1\n\n\ndef fine():\n    return 2\n",
            "mixed.py",
            &Language::Python,
        )
        .unwrap();

    let snap = graph.snapshot();
    let fine = snap
        .functions
        .get("mixed.py::fine")
        .expect("clean function still indexes");
    assert_eq!(fine.parse_quality, ParseQuality::Clean);
    assert_ne!(fine.content_hash, 0, "content_hash was hardcoded to 0");

    let module = snap.modules.get("mixed.py::module").expect("module entity");
    assert_eq!(
        module.parse_quality,
        ParseQuality::Partial,
        "the file as a whole did not parse cleanly"
    );
}

/// An unchanged file must not churn the projection — that is the whole point
/// of the diff.
#[test]
fn test_update_file_skips_unchanged_functions() {
    let source = "def f():\n    return 1\n\n\ndef g():\n    return 2\n";
    let graph = CodeGraph::new(GraphConfig::default());
    graph.update_file("same.py", Some(source), None).unwrap();
    // Second identical update: nothing changed, nothing to do. (The
    // first update converges any seed spelling to canonical — F14.)
    let outcome = graph.update_file("same.py", Some(source), None).unwrap();
    let (added, removed) = (outcome.entities_added, outcome.entities_removed);
    assert_eq!((added, removed), (0, 0), "nothing changed, nothing to do");
}

/// A legacy-spelled seed (pre-5.1 dot prefix) converges to canonical on the
/// first update: stale spellings removed, canonical inserted — once.
#[test]
fn test_update_file_converges_legacy_ids_to_canonical() {
    let source = "def f():\n    return 1\n";
    let graph = CodeGraph::new(GraphConfig::default());
    graph
        .index_file(source, "./same.py", &Language::Python)
        .unwrap();
    assert!(graph.snapshot().functions.contains_key("./same.py::f"));

    let outcome = graph.update_file("same.py", Some(source), None).unwrap();
    let snap = graph.snapshot();
    assert!(snap.functions.contains_key(&canon_id("same.py", "f")));
    assert!(
        !snap.functions.contains_key("./same.py::f"),
        "stale spelling must not survive beside the canonical one"
    );
    // One-time migration noise: the legacy function AND module are removed,
    // the canonical pair inserted (module inserts don't bump `added`, so the
    // counters read (1, 2) — asserted loosely).
    assert!(outcome.entities_added >= 1);
    assert!(outcome.entities_removed >= 1);

    // And now it is stable: a further identical update is a no-op.
    let again = graph.update_file("same.py", Some(source), None).unwrap();
    assert_eq!((again.entities_added, again.entities_removed), (0, 0));
}

#[test]
fn test_update_file_adds_entities() {
    let graph = CodeGraph::new(GraphConfig::default());

    // Seed through update_file (canonical ids throughout — F14).
    graph
        .update_file("mod.py", Some("def foo(): pass\ndef bar(): pass\n"), None)
        .unwrap();
    let initial = graph.snapshot().functions.len();
    assert_eq!(initial, 2, "Expected 2 functions");

    // Update: change bar, add baz — foo unchanged → diff skips it
    let result = graph.update_file(
        "mod.py",
        Some("def foo(): pass\ndef bar(): return 42\ndef baz(): pass\n"),
        None,
    );
    assert!(result.is_ok(), "update_file error: {:?}", result.err());
    let outcome = result.unwrap();
    let (added, removed) = (outcome.entities_added, outcome.entities_removed);

    // baz is new, and bar's changed body rewrites it under the same id.
    assert!(added >= 1, "Should insert at least 1, got {}", added);
    // The assertion here was `removed >= 0`, always true on a usize. The
    // real count is 0: a changed entity is rewritten in place, so nothing
    // is retired. Removal is what happens when an entity disappears —
    // test_update_file_removes_entities covers that.
    assert_eq!(removed, 0, "a rewritten entity is not a removed one");

    let snap = graph.snapshot();
    assert!(
        snap.functions.contains_key(&canon_id("mod.py", "baz")),
        "Should have new baz"
    );
    assert!(
        snap.functions.contains_key(&canon_id("mod.py", "foo")),
        "Foo should survive"
    );
}
#[test]
fn test_update_file_removes_entities() {
    let graph = CodeGraph::new(GraphConfig::default());

    graph
        .update_file(
            "animals.py",
            Some("class Dog: pass\nclass Cat: pass\n"),
            None,
        )
        .unwrap();
    assert_eq!(graph.snapshot().classes.len(), 2);

    // Remove Cat — Dog unchanged → 0 inserts, 1 remove
    let result = graph.update_file("animals.py", Some("class Dog: pass\n"), None);
    assert!(result.is_ok(), "update_file error: {:?}", result.err());
    let outcome = result.unwrap();
    let (added, removed) = (outcome.entities_added, outcome.entities_removed);

    // Diff semantics: Dog unchanged → 0 insert, Cat gone → 1 remove
    assert_eq!(added, 0, "Should add 0 (Dog unchanged), got {}", added);
    assert_eq!(removed, 1, "Should remove 1 (Cat), got {}", removed);

    let snap = graph.snapshot();
    assert!(snap.classes.contains_key(&canon_id("animals.py", "Dog")));
    assert!(!snap.classes.contains_key(&canon_id("animals.py", "Cat")));
}

// ── Retirement reaches the ledger (plan §1.1) ────────────────────────

/// Live concept ids in the attached store, so a removal can be checked
/// where it used never to land: the projection dropped the entity and the
/// ledger kept claiming it was still there.
fn live_ids(graph: &CodeGraph) -> Vec<String> {
    let store = graph.store.as_ref().expect("store attached");
    store.live_concept_ids().unwrap()
}

#[test]
fn test_update_file_retires_removed_entities_in_the_store() {
    let (graph, _dir) = graph_with_temp_store();
    graph
        .update_file(
            "animals.py",
            Some("class Dog: pass\nclass Cat: pass\n"),
            None,
        )
        .unwrap();
    assert!(live_ids(&graph).contains(&canon_id("animals.py", "Cat")));

    graph
        .update_file("animals.py", Some("class Dog: pass\n"), None)
        .unwrap();

    let live = live_ids(&graph);
    assert!(
        !live.contains(&canon_id("animals.py", "Cat")),
        "the deleted class must not stay current: {:?}",
        live
    );
    assert!(
        live.contains(&canon_id("animals.py", "Dog")),
        "the surviving class must stay current: {:?}",
        live
    );
}

#[test]
fn test_remove_file_entities_retires_the_whole_file() {
    let (graph, _dir) = graph_with_temp_store();
    index_source(&graph, "def a(): pass\ndef b(): pass\n", "gone.py");
    index_source(&graph, "def c(): pass\n", "kept.py");

    let mut projection = (*graph.snapshot()).clone();
    let removed = graph.remove_file_entities(&mut projection, "gone.py");
    graph.commit_projection(projection);

    assert!(!removed.is_empty());
    let live = live_ids(&graph);
    assert!(
        live.iter().all(|id| !id.starts_with("gone.py")),
        "nothing from the deleted file stays current: {:?}",
        live
    );
    assert!(live.contains(&"kept.py::c".to_string()), "{:?}", live);
}

/// A graph with no store must not panic or complain when entities go.
#[test]
fn test_removal_without_a_store_is_silent() {
    let graph = CodeGraph::new(GraphConfig::default());
    graph
        .update_file(
            "animals.py",
            Some("class Dog: pass\nclass Cat: pass\n"),
            None,
        )
        .unwrap();
    let outcome = graph
        .update_file("animals.py", Some("class Dog: pass\n"), None)
        .unwrap();
    assert_eq!(outcome.entities_removed, 1);
}

// ── Deletions reach the graph (plan §1.3) ────────────────────────────

#[test]
fn test_remove_file_drops_entities_and_retires_them() {
    let (graph, _dir) = graph_with_temp_store();
    index_source(&graph, "def a(): pass\ndef b(): pass\n", "gone.py");
    index_source(&graph, "def c(): pass\n", "kept.py");

    let removed = graph.remove_file("gone.py");

    assert!(removed.len() >= 2, "functions + module, got {:?}", removed);
    let snap = graph.snapshot();
    assert!(!snap.functions.contains_key("gone.py::a"));
    assert!(snap.functions.contains_key("kept.py::c"));
    let live = graph.store.as_ref().unwrap().live_concept_ids().unwrap();
    assert!(
        live.iter().all(|id| !id.starts_with("gone.py")),
        "{:?}",
        live
    );
}

/// The file→module mapping outlived the module, so a recreated file
/// resolved to an id that was no longer in the projection.
#[test]
fn test_remove_file_clears_the_file_to_module_mapping() {
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "def a(): pass\n", "gone.py");

    graph.remove_file("gone.py");

    let snap = graph.snapshot();
    assert!(!snap
        .file_to_modules
        .contains_key(&std::path::PathBuf::from("gone.py")));
}

#[test]
fn test_remove_file_is_a_no_op_for_an_unknown_file() {
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "def a(): pass\n", "kept.py");

    assert!(graph.remove_file("never_indexed.py").is_empty());
    assert!(graph.snapshot().functions.contains_key("kept.py::a"));
}

#[test]
fn remove_file_fallback_clears_every_kind_map() {
    // R2-4: the file_to_modules-miss fallback collected only functions +
    // classes by a bare normalized prefix that can never match canonical
    // ids -- constants, aliases, imports and the module itself survived as
    // search ghosts. Simulate key-form drift and assert every map is clean.
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(
        &graph,
        "import os\nX = 1\ntype Alias = int\ndef f():\n    pass\nclass C:\n    pass\n",
        "gone.py",
    );
    let mut projection = (*graph.snapshot()).clone();
    assert!(projection.functions.keys().any(|id| id.ends_with("::f")));
    // Simulate key-form drift: the module lookup misses.
    projection.file_to_modules.clear();
    let removed = graph.remove_file_entities(&mut projection, "gone.py");
    assert!(
        !removed.is_empty(),
        "fallback must collect by canonical prefix"
    );
    for id in projection
        .functions
        .keys()
        .chain(projection.classes.keys())
        .chain(projection.constants.keys())
        .chain(projection.type_aliases.keys())
        .chain(projection.imports.keys())
        .chain(projection.modules.keys())
    {
        assert!(!id.contains("gone.py"), "ghost left in maps: {id}");
    }
}

#[test]
fn update_file_refreshes_same_line_import_bindings() {
    // R2-17: import ids are line-stable (`file::import@N`), so an
    // ID-presence check kept the stale binding forever — the scoped path
    // re-resolved `combine_r2` against an import that still said `combine`
    // (disk healed by the rename plan, memory stuck on external::).
    // Same-line binding edits must replace the stored import.
    let graph = CodeGraph::new(GraphConfig::default());
    graph
        .index_file(
            "from app import combine\ndef run():\n    return combine()\n",
            "main.py",
            &Language::Python,
        )
        .unwrap();
    graph
        .update_file(
            "main.py",
            Some("from app import combine_r2\ndef run():\n    return combine_r2()\n"),
            None,
        )
        .unwrap();
    let snap = graph.snapshot();
    let names: Vec<String> = snap
        .imports
        .values()
        .flat_map(|i| match &i.kind {
            crate::types::ImportKind::FromImport { names, .. } => {
                names.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>()
            }
            _ => vec![],
        })
        .collect();
    assert!(
        names.iter().any(|n| n == "combine_r2"),
        "stored import must carry the new binding, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "combine"),
        "stale binding must be gone, got {names:?}"
    );
}

#[test]
fn repeated_updates_do_not_duplicate_import_membership() {
    // R2-17: the per-unit import insert appended to module.imports without
    // dedup, so every update_file added another entry per import id and the
    // rename planner emitted N identical edits at one span — applied twice,
    // the second lands on shifted bytes and the parse check rolls back.
    let graph = CodeGraph::new(GraphConfig::default());
    graph
        .index_file("from app import combine\n", "main.py", &Language::Python)
        .unwrap();
    graph
        .update_file("main.py", Some("from app import combine_r2\n"), None)
        .unwrap();
    graph
        .update_file("main.py", Some("from app import combine_r3\n"), None)
        .unwrap();
    let snap = graph.snapshot();
    let module = snap
        .modules
        .values()
        .find(|m| m.path.to_string_lossy().ends_with("main.py"))
        .expect("main module present");
    let mut seen = std::collections::BTreeSet::new();
    for id in &module.imports {
        assert!(
            seen.insert(id.clone()),
            "duplicate import membership after updates: {id}"
        );
    }
}

// ── §2.5: class-level field defaults are construction evidence ──────────────

/// A class-body default `session_interface: Base = Sub()` constructs the
/// instance at class-definition time — the class must count as instantiated
/// even though no *function body* contains the call, and `type[X]`
/// annotations with a concrete default dispatch constructions through the
/// default (`provider: type[Base] = Base` → `self.provider()` constructs).
#[test]
fn class_defaults_construct_and_class_valued_attrs_call_through() {
    let proj = snapshot_from(&[(
        "class Base:\n    def serve(self):\n        return 1\n\n\
         class Sub(Base):\n    def serve(self):\n        return 2\n\n\
         class Flask:\n    session_interface: Base = Sub()\n    provider: type[Base] = Base\n\n    def choose(self):\n        return self.provider()\n\n    def run(self):\n        print(self.session_interface.serve())\n",
        "app.py",
    )]);

    let instantiated = crate::graph::rta_lite::instantiated_classes(&proj);
    assert!(
        instantiated.contains("app.py::Sub"),
        "a class-body `= Sub()` default must record construction; got {instantiated:?}"
    );
    assert!(
        instantiated.contains("app.py::Base"),
        "the class-valued `provider` default names Base as the constructed class"
    );

    // `self.provider()` resolves to Constructor(Base), not an unknown method.
    let choose = proj.functions.get(&fn_id_of(&proj, "choose")).unwrap();
    assert!(
        choose
            .resolved_calls
            .contains(&crate::types::ResolvedCall::Constructor(
                "app.py::Base".into()
            )),
        "self.provider() with `provider: type[Base] = Base` must be a constructor edge; got {:?}",
        choose.resolved_calls
    );

    // `self.session_interface.serve()` — receiver typed through the
    // annotated class field — stays a direct method edge.
    let run = proj.functions.get(&fn_id_of(&proj, "run")).unwrap();
    assert!(
        run.resolved_calls
            .contains(&crate::types::ResolvedCall::Function(
                "app.py::Base.serve".into()
            )),
        "serve must resolve through the annotated field; got {:?}",
        run.resolved_calls
    );

    // And `Sub.serve` is NOT rta-dead: Sub is constructed in the class body.
    let findings = crate::graph::deadcode::detect_dead(&proj, Default::default());
    assert!(
        !findings.iter().any(|f| f.entity_id == "app.py::Sub.serve"
            && f.kind == crate::graph::deadcode::DeadKind::RtaDead),
        "Sub is instantiated; its override must not be rta-dead: {:?}",
        findings
            .iter()
            .filter(|f| f.entity_id.ends_with("::serve"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn receiver_typed_through_imported_annotation_and_single_subclass_override() {
    let base_src = concat!(
        "class SessionInterface:
",
        "    def get_cookie_name(self, app):
",
        "        return \"x\"
",
        "
",
        "
",
        "class SecureSessionInterface(SessionInterface):
",
        "    def save_session(self, app, session, response):
",
        "        return 1
",
    );
    let app_src = concat!(
        "from base import SessionInterface
",
        "
",
        "
",
        "class Flask:
",
        "    session_interface: SessionInterface = SecureSessionInterface()
",
        "
",
        "    def process_response(self, app, response):
",
        "        self.session_interface.save_session(app, response, app)
",
        "
",
        "    def read_name(self, app):
",
        "        return self.session_interface.get_cookie_name(app)
",
    );
    let proj = snapshot_from(&[(base_src, "base.py"), (app_src, "app.py")]);

    // `save_session` exists only on the subclass; the receiver's declared type
    // is the imported base. Single-subclass dispatch must bind to the override.
    let process_response = proj
        .functions
        .get(&fn_id_of(&proj, "process_response"))
        .unwrap();
    assert!(
        process_response.resolved_calls.contains(
            &crate::types::ResolvedCall::Function("base.py::SecureSessionInterface.save_session".into())
        ),
        "declared-type receiver with a single overriding subclass must bind to the override; got {:?}",
        process_response.resolved_calls
    );

    // A method on the declared base itself binds directly.
    let read_name = proj.functions.get(&fn_id_of(&proj, "read_name")).unwrap();
    assert!(
        read_name
            .resolved_calls
            .contains(&crate::types::ResolvedCall::Function(
                "base.py::SessionInterface.get_cookie_name".into()
            )),
        "base-typed receiver must bind the base method; got {:?}",
        read_name.resolved_calls
    );
}
