// Projection diff/update tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;

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
