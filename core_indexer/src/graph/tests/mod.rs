    use super::*;
    // Explicit imports for items no longer glob-visible from this module's new
    // location (they were in scope via the old single-file graph.rs header).
    use petgraph::graph::NodeIndex;

    mod query_compile_tests;
    mod import_graph_tests;
    mod call_graph_tests;
    mod mro_tests;
    mod traversal_tests;
    mod inheritance_tests;
    mod embedding_tests;
    mod persistence_tests;
    mod projection_tests;

    fn make_call_node(g: &mut CallGraph, id: &str) -> NodeIndex {
        if let Some(existing) = g.path_to_node.get(id) {
            return *existing;
        }
        let idx = g.graph.add_node(CallNode {
            entity_id: id.into(),
            qualified_name: format!("mod.{}", id),
        });
        g.path_to_node.insert(id.into(), idx);
        idx
    }

    fn make_call_edge(g: &mut CallGraph, from: &str, to: &str) {
        let a = make_call_node(g, from);
        let b = make_call_node(g, to);
        g.graph.add_edge(a, b, CallEdge {
            confidence: 0.95,
            resolution_method: ResolutionMethod::StackGraph,
            call_site_span: ByteSpan { start: 0, end: 1 },
            args_span: None,
        });
    }

    #[test]
    fn test_kotlin_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Person(val name: String) { fun greet() {} }\n", "Person.kt");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Person"),
                "Should have Kotlin class Person");
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should have Kotlin function greet");
    }

    #[test]
    fn test_kotlin_call_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "fun foo() { bar() }\nfun bar() {}\n", "fn.kt");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "foo"),
                "Should have Kotlin function foo");
    }

    // ── Import Parsing & Cross-File Resolution Tests ──────────────

    /// Helper: index a source string with language auto-detection from extension.
    fn index_source(graph: &CodeGraph, source: &str, file_path: &str) {
        let lang = Language::from_extension(
            std::path::Path::new(file_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("py")
        );
        graph.index_file(source, file_path, &lang).unwrap();
    }

    #[test]
    fn test_typescript_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "function hello(name: string): string {\n  return `Hello ${name}`;\n}\n\nconst add = (a: number, b: number): number => a + b;\n",
            "src/util.ts");

        let snap = graph.snapshot();
        // hello should be indexed
        assert!(snap.functions.values().any(|f| f.name == "hello"),
                "Should have function hello");
    }

    #[test]
    fn test_typescript_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Animal {\n  speak() { return 'hi'; }\n  move() { this.speak(); }\n}\n",
            "src/animal.ts");

        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Animal"),
                "Should have class Animal");
        assert!(snap.functions.values().any(|f| f.name == "speak"),
                "Should have method speak");
    }

    #[test]
    fn test_member_expression_base_is_stringified_not_dropped() {
        // Phase 2 caveat-1: TS/JS `extends X.Y` (member_expression) and simple
        // `extends E` were BOTH silently dropped by extract_base_classes — the
        // superclass lives under `class_heritage → extends_clause value:`, not a
        // `superclasses`/`superclass` field on `class_declaration`. Bases are now
        // captured (qualified ones as dotted names).
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class X { }
class Sub extends X.Y { }
class D extends E { }
class I implements G.H, J { }
",
            "src/qualified.ts");
        let snap = graph.snapshot();

        let sub = snap.classes.values().find(|c| c.name == "Sub")
            .expect("Sub should be indexed");
        assert!(sub.bases.iter().any(|b| b.name == "X.Y"),
                "member_expression base should be captured as X.Y, got {:?}",
                sub.bases.iter().map(|b| b.name.clone()).collect::<Vec<_>>());

        let d = snap.classes.values().find(|c| c.name == "D")
            .expect("D should be indexed");
        assert!(d.bases.iter().any(|b| b.name == "E"),
                "simple TS extends base should be captured as E, got {:?}",
                d.bases.iter().map(|b| b.name.clone()).collect::<Vec<_>>());

        let i = snap.classes.values().find(|c| c.name == "I")
            .expect("I should be indexed");
        assert!(i.bases.iter().any(|b| b.name == "G.H")
                && i.bases.iter().any(|b| b.name == "J"),
                "implements bases should be captured as G.H and J, got {:?}",
                i.bases.iter().map(|b| b.name.clone()).collect::<Vec<_>>());
    }

    // ── Go Indexing Tests ──────────────────────────────────────

    #[test]
    fn test_go_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "package main\nfunc hello(name string) string { return \"hi\" }\n",
            "main.go");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "hello"),
                "Should have Go function hello");
    }

    #[test]
    fn test_go_struct_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "package main\ntype Dog struct { Name string }\nfunc (d *Dog) Bark() {}\n",
            "dog.go");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Dog"),
                "Should have Go struct Dog");
        assert!(snap.functions.values().any(|f| f.name == "Bark"),
                "Should have Go method Bark");
    }

    // ── Java Indexing Tests ────────────────────────────────────

    #[test]
    fn test_java_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Cat { void meow() { this.eat(); } void eat() {} }\n",
            "Cat.java");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Cat"),
                "Should have Java class Cat");
        assert!(snap.functions.values().any(|f| f.name == "meow"),
                "Should have Java method meow");
    }

    #[test]
    fn test_java_call_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Foo { void bar() { baz(); } void baz() {} }\n",
            "Foo.java");
        let snap = graph.snapshot();
        let bar = snap.functions.values().find(|f| f.name == "bar");
        assert!(bar.is_some(), "Should have bar");
        assert!(!bar.unwrap().calls.is_empty(), "bar should have calls");
    }

    // ── C++ Indexing Tests ─────────────────────────────────────

    #[test]
    fn test_cpp_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "int add(int a, int b) { return a + b; }\n",
            "math.cpp");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "add"),
                "Should have C++ function add");
    }

    #[test]
    fn test_cpp_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Widget { public: void render() {} void paint() {} };\n",
            "widget.cpp");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Widget"),
                "Should have C++ class Widget");
        assert!(snap.functions.values().any(|f| f.name == "render"),
                "Should have C++ method render");
    }

    // ── MRO / C3 Linearization Tests ────────────────────────────

    fn snapshot_from(sources: &[(&str, &str)]) -> ProjectedGraph {
        let graph = CodeGraph::new(GraphConfig::default());
        for (src, path) in sources {
            let lang = Language::from_extension(
                std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("py"),
            );
            graph.index_file(src, path, &lang).unwrap();
        }
        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_imports(&mut projection);
        graph.compute_all_mro(&mut projection);
        graph.resolve_class_hierarchy(&mut projection);
        graph.resolve_overrides(&mut projection);
        graph.resolve_all_calls(&mut projection);
        projection
    }

    fn fn_id_of(proj: &ProjectedGraph, name: &str) -> String {
        proj.functions.iter().find(|(_, f)| f.name == name).map(|(id, _)| id.clone())
            .unwrap_or_else(|| panic!("function `{}` should be indexed", name))
    }

    #[test]
    fn test_ruby_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Animal\n  def speak; end\nend\n", "animal.rb");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Animal"),
                "Should have Ruby class Animal");
        assert!(snap.functions.values().any(|f| f.name == "speak"),
                "Should have Ruby method speak");
    }

    #[test]
    fn test_ruby_module_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "module Utilities\n  def self.format; end\nend\n", "utils.rb");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Utilities"),
                "Should have Ruby module Utilities");
    }

    // ── PHP Indexing Tests ──────────────────────────────────────

    #[test]
    fn test_php_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "<?php class User { function login() {} }\n", "User.php");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "User"),
                "Should have PHP class User");
    }

    #[test]
    fn test_php_call_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "<?php function foo() { bar(); }\n", "fn.php");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "foo"),
                "Should have PHP function foo");
    }

    // ── C# Indexing Tests ───────────────────────────────────────

    #[test]
    fn test_csharp_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Service { void Run() {} }\n", "Service.cs");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Service"),
                "Should have C# class Service");
    }

    #[test]
    fn test_csharp_call_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class T { void A() { B(); } void B() {} }\n", "T.cs");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "A"),
                "Should have C# method A");
    }

    // ── Go Receiver / Method Mapping ────────────────────────────

    #[test]
    fn test_go_method_receiver() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "package main\ntype Dog struct { Name string }\nfunc (d *Dog) Bark() {}\n",
            "dog.go");
        let snap = graph.snapshot();
        if let Some(bark) = snap.functions.values().find(|f| f.name == "Bark") {
            assert!(bark.parent_class.is_some(),
                    "Go method Bark should have parent_class (receiver type Dog)");
        }
    }

    // ── Embedding Pipeline Tests ────────────────────────────────

    #[test]
    fn test_import_parsing_from_import() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "from os import path\ndef foo(): path.join('x')\n", "mod.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_all_calls(&mut projection);

        // The import should create an Import entity with FromImport kind
        let imports: Vec<_> = projection.imports.values().collect();
        assert!(!imports.is_empty(), "Should have at least one import");
        let import = &imports[0];
        match &import.kind {
            ImportKind::FromImport { module, names } => {
                assert_eq!(module, "os");
                assert_eq!(names.len(), 1);
                assert_eq!(names[0].0, "path");
            }
            other => panic!("Expected FromImport, got {:?}", other),
        }
    }

    #[test]
    fn test_import_parsing_module_import() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "import os.path as p\ndef foo(): p.join('x')\n", "mod.py");

        let projection = graph.snapshot();
        let imports: Vec<_> = projection.imports.values().collect();
        assert!(!imports.is_empty());
        match &imports[0].kind {
            ImportKind::ModuleImport { module, alias } => {
                assert_eq!(module, "os.path");
                assert_eq!(alias.as_deref(), Some("p"));
            }
            other => panic!("Expected ModuleImport, got {:?}", other),
        }
    }

    #[test]
    fn test_cross_file_resolution_same_dir() {
        let graph = CodeGraph::new(GraphConfig::default());

        // module_a defines helper
        index_source(&graph, "def helper(x): return x * 2\n", "src/module_a.py");
        // module_b imports and calls helper
        index_source(&graph, "from module_a import helper\ndef process(): return helper(42)\n", "src/module_b.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_all_calls(&mut projection);
        graph.commit_projection(projection);

        // process should call helper from module_a
        let process_id = "src/module_b.py::process";
        let helper_id = "src/module_a.py::helper";

        let callees = graph.callees_of(process_id);
        assert!(!callees.is_empty(),
                "process should have at least one callee, got {:?}", callees);
        assert!(callees.contains(&helper_id.to_string()),
                "process should call {}, got {:?}", helper_id, callees);

        let callers = graph.callers_of(helper_id);
        assert!(callers.contains(&process_id.to_string()),
                "helper should be called by process, got {:?}", callers);
    }

    #[test]
    fn test_cross_file_resolution_nested_package() {
        let graph = CodeGraph::new(GraphConfig::default());

        // Simulate coderadar.config.Config
        index_source(&graph, "class Config:\n    pass\n",
                     "py_agent/src/coderadar/config.py");
        // pipeline imports Config
        index_source(&graph,
                     "from coderadar.config import Config\ndef make_cfg(): return Config()\n",
                     "py_agent/src/coderadar/pipeline.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_all_calls(&mut projection);
        graph.commit_projection(projection);

        let make_cfg_id = "py_agent/src/coderadar/pipeline.py::make_cfg";
        let callees = graph.callees_of(make_cfg_id);
        // Config() is a constructor call — should resolve to the class in config.py
        assert!(!callees.is_empty(),
                "make_cfg should have at least one callee, got {:?}", callees);
    }

    // ── Rust Indexing & Method Resolution Tests ──────────────────

    #[test]
    fn test_rust_struct_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "pub struct Foo { x: i32 }\nimpl Foo { pub fn new() -> Self { Foo { x: 0 } } }\n",
            "src/lib.rs");

        let projection = graph.snapshot();
        // Should have struct Foo and method Foo::new
        let struct_id = "src/lib.rs::Foo";
        assert!(projection.classes.contains_key(struct_id),
                "Should have struct Foo");

        let method_id = "src/lib.rs::Foo.new";
        assert!(projection.functions.contains_key(method_id),
                "Should have method Foo::new");

        let method = projection.functions.get(method_id).unwrap();
        assert!(method.parent_class.is_some(),
                "new() should have parent_class set to Foo");
    }

    #[test]
    fn test_rust_method_resolution() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "pub struct Foo { x: i32 }\nimpl Foo { pub fn bar(&self) -> i32 { self.baz() } pub fn baz(&self) -> i32 { 42 } }\n",
            "src/lib.rs");

        let mut projection = (*graph.snapshot()).clone();
        graph.resolve_all_calls(&mut projection);
        graph.commit_projection(projection);

        let bar_id = "src/lib.rs::Foo.bar";
        let baz_id = "src/lib.rs::Foo.baz";

        // bar() calls self.baz() — should resolve via class_methods
        let callees = graph.callees_of(bar_id);
        assert!(callees.contains(&baz_id.to_string()),
                "bar() should call baz() via self, got {:?}", callees);
    }

    #[test]
    fn test_class_methods_populated_27() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "class Foo:\n    def bar(self): pass\n    def baz(self): pass\n",
            "foo.py");

        let mut projection = (*graph.snapshot()).clone();
        graph.populate_class_methods(&mut projection);

        let foo = projection.classes.values().find(|c| c.name == "Foo").unwrap();
        assert_eq!(foo.methods.len(), 2, "class.methods should list 2 methods");
        let names: Vec<&str> = foo.methods.iter()
            .filter_map(|mid| projection.functions.get(mid))
            .map(|f| f.name.as_str())
            .collect();
        assert!(names.contains(&"bar") && names.contains(&"baz"),
                "class.methods should list bar and baz, got {:?}", names);
        // Deterministic ordering (sorted by EntityId).
        assert!(foo.methods.windows(2).all(|w| w[0] <= w[1]),
                "methods should be sorted");
    }

    #[test]
    fn test_swift_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "func greet(name: String) -> String { return \"Hi\" }\n", "test.swift");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should index Swift function greet; functions={:?}",
                snap.functions.values().map(|f| f.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_swift_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Dog { func bark() {} }\nstruct Cat { var age: Int }\n", "animals.swift");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Dog"),
                "Should index Swift class Dog; classes={:?}",
                snap.classes.values().map(|c| c.name.clone()).collect::<Vec<_>>());
        assert!(snap.functions.values().any(|f| f.name == "bark"),
                "Should index Swift method bark");
    }

    #[test]
    fn test_scala_class_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class User(name: String) { def greet(): Unit = {} }\ntrait Service { def run(): Unit }\n", "user.scala");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "User"),
                "Should index Scala class User");
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should index Scala method greet");
    }

    #[test]
    fn test_lua_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "function greet(name)\n  return name\nend\n", "test.lua");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should index Lua function greet");
    }

    #[test]
    fn test_lua_table_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "local M = {}\nfunction M.setup() end\n", "mod.lua");
        let snap = graph.snapshot();
        // Lua tables captured as classes
        assert!(snap.classes.values().any(|c| c.name == "M") || snap.functions.len() > 0,
                "Should have Lua entities");
    }

    #[test]
    fn test_elixir_module_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "defmodule MyApp.User do\n  def greet(name) do\n    \"Hello \" <> name\n  end\nend\n", "user.ex");
        let snap = graph.snapshot();
        assert!(snap.modules.len() > 0, "Should index the module");
        // v3.6: def/defp extraction — verify function entity
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should extract greet function from def block; functions={:?}",
                snap.functions.values().map(|f| f.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_zig_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "fn add(a: i32, b: i32) i32 {\n    return a + b;\n}\n", "test.zig");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "add"),
                "Should index Zig function add; functions={:?}",
                snap.functions.values().map(|f| f.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_zig_struct_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "const Point = struct { x: f32, y: f32 };\n", "geom.zig");
        let snap = graph.snapshot();
        assert!(snap.classes.values().any(|c| c.name == "Point"),
                "Should index Zig struct Point; classes={:?}",
                snap.classes.values().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_r_function_indexing() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "greet <- function(name) {\n  paste('Hi', name)\n}\n", "test.R");
        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "greet"),
                "Should index R function greet; functions={:?}",
                snap.functions.values().map(|f| f.name.clone()).collect::<Vec<_>>());
    }

    // ── v3.6: Function-as-Value Reference Capture Tests ────────────

    #[test]
    fn test_fn_ref_assignment_callback() {
        // Python pattern: `on_click = self.handle_click` → fn-ref from on_click to handle_click
        let graph = CodeGraph::new(GraphConfig::default());
        let source = "class Widget:\n  def handle_click(self): pass\n  def register(self):\n    self.on_click = self.handle_click\n";
        index_source(&graph, source, "widget.py");

        let snap = graph.snapshot();
        assert!(snap.functions.values().any(|f| f.name == "handle_click"),
                "should have handle_click function");
        let register = snap.functions.values()
            .find(|f| f.name == "register");
        assert!(register.is_some(), "should have register function");
        let register = register.unwrap();
        let has_handle_click_ref = register.calls.iter().any(|c| c.name == "handle_click");
        assert!(has_handle_click_ref,
                "register should have fn-ref to handle_click; calls={:?}",
                register.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_fn_ref_return_value() {
        // Python pattern: `return handler` → fn-ref
        let graph = CodeGraph::new(GraphConfig::default());
        let source = "def greeter(): pass\ndef get_handler():\n    return greeter\n";
        index_source(&graph, source, "handlers.py");

        let snap = graph.snapshot();
        let get_handler = snap.functions.values()
            .find(|f| f.name == "get_handler");
        assert!(get_handler.is_some(), "should have get_handler function");
        let get_handler = get_handler.unwrap();
        assert!(get_handler.calls.iter().any(|c| c.name == "greeter"),
                "get_handler should have fn-ref to greeter; calls={:?}",
                get_handler.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_fn_ref_no_false_positives() {
        // Local variable assignment should NOT create fn-ref
        let graph = CodeGraph::new(GraphConfig::default());
        let source = "def foo():\n    x = 42\n    y = 'hello'\n    return x\n";
        index_source(&graph, source, "locals.py");

        let snap = graph.snapshot();
        let foo = snap.functions.values()
            .find(|f| f.name == "foo");
        assert!(foo.is_some(), "should have foo function");
        let foo = foo.unwrap();
        assert!(foo.calls.is_empty(),
                "foo should have no fn-ref calls; got {:?}",
                foo.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_fn_ref_argument_list() {
        // Argument-list fn-ref: `register_callback(handler)` → handler is fn-ref
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "def handler(x): pass\ndef register_callback(fn): fn(42)\ndef setup():\n    register_callback(handler)\n",
            "callback.py");

        let snap = graph.snapshot();
        let setup = snap.functions.values()
            .find(|f| f.name == "setup");
        assert!(setup.is_some(), "should have setup function");
        let setup = setup.unwrap();
        assert!(setup.calls.iter().any(|c| c.name == "handler"),
                "setup should have fn-ref to handler via argument; calls={:?}",
                setup.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_fn_ref_dict_values() {
        // Dict value fn-ref: `{"key": handler}` → handler is fn-ref
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "def handler(x): pass\ndef make_registry():\n    return {'cb': handler}\n",
            "registry.py");

        let snap = graph.snapshot();
        let make_reg = snap.functions.values()
            .find(|f| f.name == "make_registry");
        assert!(make_reg.is_some(), "should have make_registry function");
        let make_reg = make_reg.unwrap();
        assert!(make_reg.calls.iter().any(|c| c.name == "handler"),
                "make_registry should have fn-ref to handler from dict value; calls={:?}",
                make_reg.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn test_literal_receiver_skipped() {
        // Calls on literal receivers like "str".method() should be skipped
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "def foo():\n    x = 'hello'.upper()\n    y = 42.to_bytes(2, 'big')\n",
            "literal.py");

        let snap = graph.snapshot();
        let foo = snap.functions.values()
            .find(|f| f.name == "foo");
        assert!(foo.is_some(), "should have foo function");
        let foo = foo.unwrap();
        // Calls on string/integer literals should be filtered — no path entries for them
        let has_literal_receiver = foo.calls.iter().any(|c| {
            c.path.iter().any(|p| p == "'hello'" || p == "42")
        });
        assert!(!has_literal_receiver,
                "literal receivers should be filtered; calls={:?}",
                foo.calls.iter().map(|c| format!("{:?}::{}", c.path, c.name)).collect::<Vec<_>>());
    }

    // ── v3.6: grammar_kind tests ─────────────────────────────────

    #[test]
    fn test_grammar_kind_python_class() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Foo: pass\n", "test.py");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Foo").unwrap();
        assert_eq!(cls.grammar_kind, "class_definition",
                   "Python class should have grammar_kind 'class_definition'");
    }

    #[test]
    fn test_grammar_kind_rust_struct() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "struct Point { x: f64, y: f64 }\n", "geom.rs");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Point").unwrap();
        assert_eq!(cls.grammar_kind, "struct_item",
                   "Rust struct should have grammar_kind 'struct_item'");
    }

    #[test]
    fn test_grammar_kind_typescript_class() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Drawable { draw(): void {} }\n", "draw.ts");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Drawable").unwrap();
        assert_eq!(cls.grammar_kind, "class_declaration",
                   "TS class should have grammar_kind 'class_declaration'");
    }

    #[test]
    fn test_grammar_kind_swift_struct() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "struct Cat { var age: Int }\n", "cat.swift");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Cat").unwrap();
        assert_eq!(cls.grammar_kind, "class_declaration/struct",
                   "Swift struct should be classified as class_declaration/struct");
    }

    #[test]
    fn test_grammar_kind_swift_class() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Dog { func bark() {} }\n", "dog.swift");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Dog").unwrap();
        assert_eq!(cls.grammar_kind, "class_declaration",
                   "Swift class should keep grammar_kind 'class_declaration'");
    }

    #[test]
    fn test_grammar_kind_zig_struct() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "const Point = struct { x: f32, y: f32 };\n", "geom.zig");
        let snap = graph.snapshot();
        let cls = snap.classes.values().find(|c| c.name == "Point").unwrap();
        assert_eq!(cls.grammar_kind, "VarDecl",
                   "Zig struct should have grammar_kind 'VarDecl'");
    }

    // ── v3.6: Synthetic edge registration ──────────────────────────

    #[test]
    fn test_fn_ref_cross_file_import() {
        // Cross-file fn-ref: `from .handlers import handle_click`
        // then `self.on_click = handle_click` in another file
        let graph = CodeGraph::new(GraphConfig::default());
        let source = concat!(
            "from .handlers import handle_click\n",
            "class Widget:\n",
            "    def register(self):\n",
            "        self.on_click = handle_click\n",
        );
        index_source(&graph, source, "widget.py");

        let snap = graph.snapshot();
        let register = snap.functions.values()
            .find(|f| f.name == "register");
        assert!(register.is_some(), "should have register function");
        let register = register.unwrap();
        assert!(register.calls.iter().any(|c| c.name == "handle_click"),
                "register should have fn-ref to imported handle_click; calls={:?}",
                register.calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    }

    // ── v3.6: module.children() convenience API ─────────────────

    #[test]
    fn test_module_children_resolution() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "class Foo:\n    def bar(self):\n        pass\n\ndef baz():\n    pass\n", "mod.py");
        let snap = graph.snapshot();

        let module = snap.modules.values().find(|m| m.path.ends_with("mod.py"));
        assert!(module.is_some(), "Should find module");
        let module = module.unwrap();

        assert!(!module.classes.is_empty(), "Module should have classes");
        assert!(!module.functions.is_empty(), "Module should have functions");

        for cid in &module.classes {
            let cls = snap.classes.get(cid);
            assert!(cls.is_some());
            assert_eq!(cls.unwrap().name, "Foo");
        }

        for fid in &module.functions {
            let func = snap.functions.get(fid);
            assert!(func.is_some());
        }
    }

    // ── v3.6: Parameter annotation + return type extraction ─────

    #[test]
    fn test_parameter_annotations_extracted() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph,
            "from typing import Optional\ndef create_user(name: str, age: int, email: Optional[str]) -> User:\n    pass\n",
            "typed.py");
        let snap = graph.snapshot();
        let func = snap.functions.values().find(|f| f.name == "create_user");
        assert!(func.is_some());
        let func = func.unwrap();

        // Parameters should have annotations (builtins filtered)
        assert_eq!(func.parameters.len(), 3);
        // name: str → annotation: None (str is builtin)
        assert_eq!(func.parameters[0].name, "name");
        assert!(func.parameters[0].annotation.is_none(), "str is builtin");
        // age: int → annotation: None (int is builtin)
        assert_eq!(func.parameters[1].name, "age");
        assert!(func.parameters[1].annotation.is_none(), "int is builtin");
        // email: Optional[str] → should have annotation (not a bare builtin)
        assert_eq!(func.parameters[2].name, "email");
        assert!(func.parameters[2].annotation.is_some(), "Optional[str] is not a bare builtin");

        // Return type: User → not builtin, should be extracted
        assert_eq!(func.return_type.as_deref(), Some("User"));
    }

    #[test]
    fn test_return_type_builtin_filtered() {
        let graph = CodeGraph::new(GraphConfig::default());
        index_source(&graph, "def get_count() -> int:\n    return 0\n", "simple.py");
        let snap = graph.snapshot();
        let func = snap.functions.values().find(|f| f.name == "get_count");
        assert!(func.is_some());
        // int is builtin → return_type should be None
        assert!(func.unwrap().return_type.is_none(), "int return type should be filtered");
    }

    // ── v3.6: Macrame temporal query tests ──────────────────────

    /// Helper: create a CodeGraph with a real Macrame store in a temp dir.
    fn graph_with_temp_store() -> (CodeGraph, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db_path = dir.path().join("test.db");
        let store = crate::storage::CodeGraphStore::open(&db_path).expect("open store");
        let graph = CodeGraph::new(GraphConfig::default()).with_store(store);
        (graph, dir)
    }
