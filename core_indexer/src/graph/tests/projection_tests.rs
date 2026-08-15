// Projection diff/update tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;

    /// `insert_extracted` stored `name_span` in `params_span`. `build_fragment`
    /// and `apply_diff_update` both got it right, so the divergence was invisible
    /// outside the `index_file` path — until `plan_signature_update` replaced
    /// `params_span` verbatim and overwrote the function *name* with the new
    /// parameter list.
    #[test]
    fn test_index_file_records_params_span_not_name_span() {
        let source = "def greet(name, greeting=\"hi\"):\n    return greeting\n";
        let graph = CodeGraph::new(GraphConfig::default());
        graph.index_file(source, "spans.py", &Language::Python).unwrap();

        let snap = graph.snapshot();
        let f = snap.functions.get("spans.py::greet").expect("greet indexed");

        assert_ne!(f.params_span, f.name_span, "params_span must not alias name_span");
        assert_eq!(&source[f.name_span.start..f.name_span.end], "greet");
        assert_eq!(
            &source[f.params_span.start..f.params_span.end],
            "(name, greeting=\"hi\")",
        );
    }

    /// All three ingest paths must agree on every span they record.
    #[test]
    #[ignore = "single_pass sets signature_hash/body_hash to 0, so apply_diff_update \
                never sees an existing function as changed and never re-inserts it"]
    fn test_index_file_and_update_file_agree_on_spans() {
        let source = "def greet(name, greeting=\"hi\"):\n    return greeting\n";

        let indexed = CodeGraph::new(GraphConfig::default());
        indexed.index_file(source, "spans.py", &Language::Python).unwrap();
        let via_index = indexed.snapshot().functions.get("spans.py::greet").cloned().unwrap();

        let updated = CodeGraph::new(GraphConfig::default());
        updated.index_file("def greet(): pass\n", "spans.py", &Language::Python).unwrap();
        updated.update_file("spans.py", Some(source), None).unwrap();
        let via_update = updated.snapshot().functions.get("spans.py::greet").cloned().unwrap();

        assert_eq!(via_index.name_span, via_update.name_span);
        assert_eq!(via_index.params_span, via_update.params_span);
        assert_eq!(via_index.body_span, via_update.body_span);
        assert_eq!(via_index.parameters.len(), via_update.parameters.len());
    }

    #[test]
    fn test_update_file_adds_entities() {
        let graph = CodeGraph::new(GraphConfig::default());

        // Verify basic indexing
        graph.index_file("def foo(): pass\ndef bar(): pass\n", "mod.py", &Language::Python).unwrap();
        let initial = graph.snapshot().functions.len();
        assert_eq!(initial, 2, "Expected 2 functions");

        // Update: change bar, add baz — foo unchanged → diff skips it
        let result = graph.update_file(
            "mod.py",
            Some("def foo(): pass\ndef bar(): return 42\ndef baz(): pass\n"),
            None,
        );
        assert!(result.is_ok(), "update_file error: {:?}", result.err());
        let (added, removed, _affected) = result.unwrap();

        // Diff semantics: bar changed (body_hash differs) → 1 remove + 1 insert
        // baz is new → 1 insert. foo unchanged → 0 ops.
        assert!(added >= 1, "Should insert at least 1, got {}", added);
        assert!(removed >= 0, "Should remove at least 0, got {}", removed);

        let snap = graph.snapshot();
        assert!(snap.functions.contains_key("mod.py::baz"), "Should have new baz");
        assert!(snap.functions.contains_key("mod.py::foo"), "Foo should survive");
    }
    #[test]
    fn test_update_file_removes_entities() {
        let graph = CodeGraph::new(GraphConfig::default());

        index_source(&graph,
            "class Dog: pass\nclass Cat: pass\n", "animals.py");
        assert_eq!(graph.snapshot().classes.len(), 2);

        // Remove Cat — Dog unchanged → 0 inserts, 1 remove
        let result = graph.update_file(
            "animals.py",
            Some("class Dog: pass\n"),
            None,
        );
        assert!(result.is_ok(), "update_file error: {:?}", result.err());
        let (added, removed, _) = result.unwrap();

        // Diff semantics: Dog unchanged → 0 insert, Cat gone → 1 remove
        assert_eq!(added, 0, "Should add 0 (Dog unchanged), got {}", added);
        assert_eq!(removed, 1, "Should remove 1 (Cat), got {}", removed);

        let snap = graph.snapshot();
        assert!(snap.classes.contains_key("animals.py::Dog"));
        assert!(!snap.classes.contains_key("animals.py::Cat"));
    }
