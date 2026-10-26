// Inheritance / base-resolution tests — moved verbatim from graph/tests/mod.rs (step 15).

use super::*;

#[test]
fn test_resolve_class_hierarchy_populates_subclasses() {
    // `class B(A)` in the SAME module — the same-module branch of
    // resolve_base_by_name must resolve A and invert it into subclasses[A]={B}.
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(
        &graph,
        "class A:\n    def foo(self): pass\nclass B(A):\n    def bar(self): pass\n",
        "hier.py",
    );
    let mut projection = (*graph.snapshot()).clone();
    graph.compute_all_mro(&mut projection);
    graph.resolve_class_hierarchy(&mut projection);

    let a_id = projection
        .classes
        .iter()
        .find(|(_, c)| c.name == "A")
        .map(|(id, _)| id.clone())
        .expect("A should be indexed");
    let subs = projection
        .subclasses
        .get(&a_id)
        .cloned()
        .unwrap_or_default();
    let sub_names: Vec<String> = subs
        .iter()
        .filter_map(|sid| projection.classes.get(sid))
        .map(|c| c.name.clone())
        .collect();
    assert!(
        sub_names.contains(&"B".to_string()),
        "subclasses[A] should contain B, got {:?}",
        sub_names
    );

    let b = projection.classes.values().find(|c| c.name == "B").unwrap();
    assert!(
        b.resolved_bases
            .iter()
            .any(|bid| projection.classes.get(bid).is_some_and(|bc| bc.name == "A")),
        "B.resolved_bases should resolve to A, got {:?}",
        b.resolved_bases
    );
}
#[test]
fn test_resolve_imports_populates_importers() {
    // `from src.c import utility` in b.py — resolve_imports must set
    // Import.resolution → Module(c) and record b in importers[c].
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "def utility(): pass\n", "src/c.py");
    index_source(
        &graph,
        "from src.c import utility\ndef helper(): utility()\n",
        "src/b.py",
    );

    let mut projection = (*graph.snapshot()).clone();
    graph.resolve_imports(&mut projection);

    let c_mod = projection
        .modules
        .iter()
        .find(|(_, m)| m.path.to_string_lossy().contains("c.py"))
        .map(|(id, _)| id.clone())
        .expect("c.py module should be indexed");
    let b_mod = projection
        .modules
        .iter()
        .find(|(_, m)| m.path.to_string_lossy().contains("b.py"))
        .map(|(id, _)| id.clone())
        .expect("b.py module should be indexed");

    let who_imports_c = projection
        .importers
        .get(&c_mod)
        .cloned()
        .unwrap_or_default();
    assert!(
        who_imports_c.contains(&b_mod),
        "importers[c.py] should contain b.py's module, got {:?}",
        who_imports_c
    );

    let b_imports: Vec<_> = projection
        .modules
        .get(&b_mod)
        .map(|m| m.imports.clone())
        .unwrap_or_default();
    let resolved_any = b_imports.iter().any(|imp_id| {
        projection
            .imports
            .get(imp_id)
            .is_some_and(|i| matches!(i.resolution, crate::types::ImportResolution::Module(_)))
    });
    assert!(
        resolved_any,
        "b.py's Import entity should resolve to Module(c), got {:?}",
        b_imports
    );
}

// ── 2.1a / 2.1b / 2.1c: base-resolution heuristics ──────────
#[test]
fn test_language_family_filters_base_candidates() {
    // Two `Base` classes in different languages; a Python caller must
    // resolve to the Python one (2.1a), not be ambiguous across C++.
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "class Base:\n    pass\n", "base.py");
    index_source(&graph, "class Base {};\n", "base.cpp");
    index_source(&graph, "class Child(Base):\n    pass\n", "main.py");
    let mut projection = (*graph.snapshot()).clone();
    graph.resolve_imports(&mut projection);
    graph.compute_all_mro(&mut projection);
    graph.resolve_class_hierarchy(&mut projection);

    let child = projection
        .classes
        .values()
        .find(|c| c.name == "Child")
        .unwrap();
    assert_eq!(
        child.resolved_bases.len(),
        1,
        "Child should resolve to one Base, got {:?}",
        child.resolved_bases
    );
    let base = projection.classes.get(&child.resolved_bases[0]).unwrap();
    let base_mod = projection.modules.get(&base.parent_module).unwrap();
    assert!(
        base_mod.path.to_string_lossy().ends_with("base.py"),
        "Child must resolve to the Python Base, not {}",
        base_mod.path.to_string_lossy()
    );
    assert!(
        projection.ambiguous_bases.is_empty(),
        "unexpected ambiguity: {:?}",
        projection.ambiguous_bases
    );
}
#[test]
fn test_ambiguous_base_emits_finding() {
    // Two Python `Service` classes in different packages; a caller with no
    // import cannot disambiguate → must emit an AmbiguousBase finding (2.1b).
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "class Service:\n    pass\n", "pkg_a/service.py");
    index_source(&graph, "class Service:\n    pass\n", "pkg_b/service.py");
    index_source(&graph, "class Consumer(Service):\n    pass\n", "main.py");
    let mut projection = (*graph.snapshot()).clone();
    graph.resolve_imports(&mut projection);
    graph.compute_all_mro(&mut projection);
    graph.resolve_class_hierarchy(&mut projection);

    let consumer = projection
        .classes
        .values()
        .find(|c| c.name == "Consumer")
        .unwrap();
    assert!(
        consumer.resolved_bases.is_empty(),
        "ambiguous base must stay unresolved"
    );
    assert_eq!(
        projection.ambiguous_bases.len(),
        1,
        "expected 1 finding, got {:?}",
        projection.ambiguous_bases
    );
    let f = &projection.ambiguous_bases[0];
    assert_eq!(f.class_name, "Consumer");
    assert_eq!(f.base_name, "Service");
    assert_eq!(
        f.candidates.len(),
        2,
        "expected 2 candidates, got {:?}",
        f.candidates
    );
}
#[test]
fn test_ts_typeonly_import_aware_base_resolution() {
    // TS `import { type PoolWorker } from '../src/mcp/query-pool'` must be
    // parsed as a relative import with the name captured, so import-aware
    // base resolution (2.1c) resolves FakeWorker → query-pool.PoolWorker.
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(&graph, "class PoolWorker {}\n", "src/mcp/query-pool.ts");
    index_source(
        &graph,
        "class PoolWorker {}\n",
        "src/resolution/resolver-pool.ts",
    );
    index_source(&graph,
            "import { type PoolWorker } from '../src/mcp/query-pool';\nclass FakeWorker implements PoolWorker {}\n",
            "__tests__/query-pool.test.ts");
    let mut projection = (*graph.snapshot()).clone();
    graph.resolve_imports(&mut projection);
    graph.compute_all_mro(&mut projection);
    graph.resolve_class_hierarchy(&mut projection);

    let fake = projection
        .classes
        .values()
        .find(|c| c.name == "FakeWorker")
        .unwrap();
    assert_eq!(
        fake.resolved_bases.len(),
        1,
        "FakeWorker should resolve PoolWorker via type-only import, got {:?}",
        fake.resolved_bases
    );
    let base = projection.classes.get(&fake.resolved_bases[0]).unwrap();
    let base_mod = projection.modules.get(&base.parent_module).unwrap();
    assert!(
        base_mod.path.to_string_lossy().ends_with("query-pool.ts"),
        "FakeWorker must resolve to query-pool.PoolWorker, got {}",
        base_mod.path.to_string_lossy()
    );
    assert!(
        projection.ambiguous_bases.is_empty(),
        "import-aware should disambiguate, got {:?}",
        projection.ambiguous_bases
    );
}
#[test]
fn test_import_aware_base_resolution() {
    // Two `PoolWorker` classes; the caller imports it from query_pool, so
    // resolution must use the import target (2.1c), not guess.
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(
        &graph,
        "class PoolWorker:\n    pass\n",
        "src/mcp/query_pool.py",
    );
    index_source(
        &graph,
        "class PoolWorker:\n    pass\n",
        "src/resolution/resolver_pool.py",
    );
    index_source(
        &graph,
        "from src.mcp.query_pool import PoolWorker\nclass FakeWorker(PoolWorker):\n    pass\n",
        "test_fake.py",
    );
    let mut projection = (*graph.snapshot()).clone();
    graph.resolve_imports(&mut projection);
    graph.compute_all_mro(&mut projection);
    graph.resolve_class_hierarchy(&mut projection);

    let fake = projection
        .classes
        .values()
        .find(|c| c.name == "FakeWorker")
        .unwrap();
    assert_eq!(
        fake.resolved_bases.len(),
        1,
        "FakeWorker should resolve PoolWorker via import, got {:?}",
        fake.resolved_bases
    );
    let base = projection.classes.get(&fake.resolved_bases[0]).unwrap();
    let base_mod = projection.modules.get(&base.parent_module).unwrap();
    assert!(
        base_mod.path.to_string_lossy().ends_with("query_pool.py"),
        "FakeWorker must resolve to query_pool.PoolWorker, got {}",
        base_mod.path.to_string_lossy()
    );
    assert!(
        projection.ambiguous_bases.is_empty(),
        "import-aware should disambiguate, got {:?}",
        projection.ambiguous_bases
    );
}
#[test]
fn test_resolve_overrides_populates_overridden_by() {
    // Base.helper overridden by Child.helper (same module). Child's MRO is
    // [Child, Base], so resolve_overrides must mark Base.helper as overridden
    // and point the Child helper's overrides_base back to Base.helper.
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(
        &graph,
        "class Base:\n    def helper(self): pass\nclass Child(Base):\n    def helper(self): pass\n",
        "overrides.py",
    );
    let mut projection = (*graph.snapshot()).clone();
    graph.compute_all_mro(&mut projection);
    graph.resolve_overrides(&mut projection);

    let base_foo = projection
        .functions
        .iter()
        .find(|(_, f)| {
            f.name == "helper"
                && f.parent_class
                    .as_deref()
                    .is_some_and(|pc| projection.classes.get(pc).is_some_and(|c| c.name == "Base"))
        })
        .map(|(id, _)| id.clone())
        .expect("Base.helper should be indexed");
    let overrides = projection
        .overridden_by
        .get(&base_foo)
        .cloned()
        .unwrap_or_default();
    assert!(
        !overrides.is_empty(),
        "Base.helper should be marked overridden by at least one Child.helper"
    );
    let child_foo = projection
        .functions
        .iter()
        .find(|(_, f)| {
            f.name == "helper"
                && f.parent_class.as_deref().is_some_and(|pc| {
                    projection
                        .classes
                        .get(pc)
                        .is_some_and(|c| c.name == "Child")
                })
        })
        .map(|(id, _)| id.clone())
        .expect("Child.helper should be indexed");
    assert_eq!(
        projection.overrides_base.get(&child_foo),
        Some(&base_foo),
        "overrides_base[Child.helper] should be Base.helper"
    );
}
#[test]
fn test_builtin_type_bases_filtered() {
    // Classes inheriting from builtin types should not track those as refs
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(
        &graph,
        "class MyException(Exception): pass\nclass MyInt(int): pass\n",
        "bases.py",
    );

    let snap = graph.snapshot();
    let exc = snap.classes.values().find(|c| c.name == "MyException");
    assert!(exc.is_some(), "should have MyException class");
    let _exc = exc.unwrap();
    // Exception is not a builtin-type (it's a class), so it stays
    // But int IS filtered by is_builtin_type
    let myint = snap.classes.values().find(|c| c.name == "MyInt");
    assert!(myint.is_some(), "should have MyInt class");
    let myint = myint.unwrap();
    // int should be filtered from bases
    assert!(
        !myint.bases.iter().any(|b| b.name == "int"),
        "int should be filtered from bases; got {:?}",
        myint
            .bases
            .iter()
            .map(|b| b.name.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_implements_partitioned_by_target_declaration() {
    // §2.6: a base resolving to an interface declaration implements;
    // a base resolving to a class extends — decided by the target, not
    // the clause spelling. (FakeWorker pins the negative: `implements`
    // against a *class* stays EXTENDS.)
    let graph = CodeGraph::new(GraphConfig::default());
    index_source(
        &graph,
        "interface Shape { area(): number; }\n\
         class Circle implements Shape { area(): number { return 0; } }\n\
         class Sub extends Circle { }\n",
        "shapes.ts",
    );
    index_source(
        &graph,
        "interface Drawable { }\nclass Sprite implements Drawable { }\n",
        "game.java",
    );
    index_source(
        &graph,
        "interface IDisposable { }\nclass Base { }\nclass C : Base, IDisposable { }\n",
        "prog.cs",
    );
    index_source(
        &graph,
        "<?php interface Countable { }\nclass Bag implements Countable { }\n",
        "bag.php",
    );
    let mut projection = (*graph.snapshot()).clone();
    graph.compute_all_mro(&mut projection);
    graph.resolve_class_hierarchy(&mut projection);

    let get = |name: &str| {
        projection
            .classes
            .values()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{name} should be indexed"))
    };
    let circle = get("Circle");
    assert!(circle.resolved_bases.is_empty(), "{circle:?}");
    assert_eq!(
        circle.resolved_interfaces,
        vec!["shapes.ts::Shape".to_string()]
    );
    let sub = get("Sub");
    assert_eq!(sub.resolved_bases, vec!["shapes.ts::Circle".to_string()]);
    assert!(sub.resolved_interfaces.is_empty());
    let sprite = get("Sprite");
    assert_eq!(
        sprite.resolved_interfaces,
        vec!["game.java::Drawable".to_string()],
        "{sprite:?}"
    );
    let prog_c = get("C");
    assert_eq!(
        prog_c.resolved_bases,
        vec!["prog.cs::Base".to_string()],
        "{prog_c:?}"
    );
    assert_eq!(
        prog_c.resolved_interfaces,
        vec!["prog.cs::IDisposable".to_string()],
        "{prog_c:?}"
    );
    let bag = get("Bag");
    assert_eq!(
        bag.resolved_interfaces,
        vec!["bag.php::Countable".to_string()],
        "{bag:?}"
    );
    // Reverse index mirrors subclasses for the upstream direction.
    let impls = projection
        .implementers
        .get("shapes.ts::Shape")
        .expect("Shape should have implementers");
    assert!(impls.contains("shapes.ts::Circle"));
    assert!(!projection.subclasses.contains_key("shapes.ts::Shape"));
}

#[test]
fn test_kotlin_interface_implements() {
    // §2.6 gap: Kotlin `interface Foo` implemented by `class Bar : Foo`
    // resolves IMPLEMENTS, not EXTENDS.
    let p = snapshot_from(&[("interface Foo { fun m() }\nclass Bar : Foo\n", "a.kt")]);
    let bar = p.classes.get("a.kt::Bar").expect("Bar indexed");
    assert!(
        bar.resolved_bases.is_empty(),
        "bases: {:?}",
        bar.resolved_bases
    );
    assert_eq!(bar.resolved_interfaces, vec!["a.kt::Foo".to_string()]);
    assert!(p.implementers["a.kt::Foo"].contains("a.kt::Bar"));
}

#[test]
fn test_go_embedded_interface_implements() {
    // §2.6 gap: Go `type J interface { I; M() }` embeds I — J implements
    // I. Structs and aliases are not interfaces, and struct embedding
    // (`struct { I }`) stays out (promotion, not inheritance).
    let p = snapshot_from(&[(
        "package p\ntype I interface { M() }\ntype J interface { I\nM() }\ntype S struct { X int }\ntype A int\n",
        "b.go",
    )]);
    let j = p.classes.get("b.go::J").expect("J indexed");
    assert!(j.resolved_bases.is_empty(), "bases: {:?}", j.resolved_bases);
    assert_eq!(j.resolved_interfaces, vec!["b.go::I".to_string()]);
    assert!(p.implementers["b.go::I"].contains("b.go::J"));
    for id in ["b.go::S", "b.go::A"] {
        let c = p.classes.get(id).expect("indexed");
        assert!(
            !c.grammar_kind.ends_with("/interface"),
            "{id}: {}",
            c.grammar_kind
        );
        assert!(c.resolved_interfaces.is_empty());
    }
}

#[test]
fn test_python_protocol_implements() {
    // §2.6 gap: `class P(Protocol)` declares an interface; `class C(P)`
    // implements it.
    let p = snapshot_from(&[(
        "from typing import Protocol\nclass P(Protocol):\n    pass\nclass C(P):\n    pass\n",
        "c.py",
    )]);
    let c = p.classes.get("c.py::C").expect("C indexed");
    assert!(c.resolved_bases.is_empty(), "bases: {:?}", c.resolved_bases);
    assert_eq!(c.resolved_interfaces, vec!["c.py::P".to_string()]);
    assert!(p.implementers["c.py::P"].contains("c.py::C"));
}

#[test]
fn test_java_annotation_decorates() {
    // §2.6 gap: Java annotations under `modifiers` are decorators;
    // `@interface Route` is a class-like target, so `@Route class C`
    // resolves DECORATES. Method-level `@Override` is captured as text
    // but leaves no edge (unresolvable, like builtins).
    let p = snapshot_from(&[(
        "@interface Route {}\n@Route public class C {\n@Override public String toString() { return \"\"; }\n}\n",
        "C.java",
    )]);
    let c = p.classes.get("C.java::C").expect("C indexed");
    assert_eq!(c.resolved_decorators, vec!["C.java::Route".to_string()]);
    assert!(p.decorated_by["C.java::Route"].contains("C.java::C"));
    let m = p
        .functions
        .get("C.java::C.toString")
        .expect("method indexed");
    assert!(
        m.decorators.iter().any(|d| d == "@Override"),
        "{:?}",
        m.decorators
    );
    assert!(m.resolved_decorators.is_empty());
}

#[test]
fn test_csharp_attribute_decorates() {
    // §2.6 gap (drive-by): C# `[Attr]` lives in `attribute_list`, not a
    // `decorator` node — `[MyAttr] class C` resolves DECORATES.
    let p = snapshot_from(&[(
        "public class MyAttr {}\n[MyAttr] public class C {}\n",
        "C.cs",
    )]);
    let c = p.classes.get("C.cs::C").expect("C indexed");
    assert_eq!(c.resolved_decorators, vec!["C.cs::MyAttr".to_string()]);
    assert!(p.decorated_by["C.cs::MyAttr"].contains("C.cs::C"));
}
