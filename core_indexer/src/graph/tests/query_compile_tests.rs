// Query compilation smoke test — moved verbatim from graph/tests/mod.rs (step 15).
// Self-contained: uses only fully-qualified paths + inline imports, so no
// `use super::*;` here.

    /// Verify that every .scm query file compiles cleanly against its
    /// tree-sitter grammar.  A query that falls back to `(comment)` silently
    /// loses entity extraction — this test catches that.
    #[test]
    fn test_all_queries_compile_without_errors() {
        use crate::extract::tagger;
        use crate::types::Language;

        let languages: Vec<(Language, &str)> = vec![
            (Language::Python, "python"),
            (Language::TypeScript, "typescript"),
            (Language::JavaScript, "javascript"),
            (Language::Rust, "rust"),
            (Language::Go, "go"),
            (Language::Java, "java"),
            (Language::C, "c"),
            (Language::Cpp, "cpp"),
            (Language::Ruby, "ruby"),
            (Language::Php, "php"),
            (Language::CSharp, "csharp"),
            (Language::Kotlin, "kotlin"),
            (Language::Swift, "swift"),
            (Language::Scala, "scala"),
            (Language::Lua, "lua"),
            (Language::Elixir, "elixir"),
            (Language::Zig, "zig"),
            (Language::R, "r"),
            (Language::Bash, "bash"),
            (Language::Dart, "dart"),
            (Language::Protobuf, "protobuf"),
            (Language::Dockerfile, "dockerfile"),
            (Language::Sql, "sql"),
            (Language::Hcl, "hcl"),
            (Language::Cmake, "cmake"),
            (Language::Graphql, "graphql"),
            (Language::Erlang, "erlang"),
            (Language::Haskell, "haskell"),
            (Language::Nix, "nix"),
            (Language::Shell, "bash"),
            (Language::Groovy, "groovy"),
            (Language::Perl, "perl"),
            (Language::SystemVerilog, "systemverilog"),
            (Language::Ocaml, "ocaml"),
            (Language::Clojure, "clojure"),
            (Language::Fsharp, "fsharp"),
            (Language::Verilog, "verilog"),
            (Language::Julia, "julia"),
            (Language::Powershell, "powershell"),
            (Language::EmacsLisp, "elisp"),
            (Language::Objc, "objc"),
        ];

        let mut failures = 0;
        let mut skipped = 0;
        for (lang, _pack_name) in &languages {
            let query_src = tagger::get_query_for_language_src(*lang);
            let ts_lang = match crate::graph::CodeGraph::ts_language(lang) {
                Some(l) => l,
                None => {
                    // The language pack (with its default `download` feature)
                    // lazily fetches grammars from GitHub releases. On fresh
                    // CI runners those downloads can be rate-limited or
                    // unavailable, so the grammar isn't loadable here. That is
                    // not a query bug — skip the compile check rather than
                    // failing the whole suite. Production `analyze` downloads
                    // and caches grammars lazily, so this only limits this
                    // static check.
                    eprintln!(
                        "SKIP {:?}: grammar not available — cannot compile-check query",
                        lang
                    );
                    skipped += 1;
                    continue;
                }
            };
            match tree_sitter::Query::new(&ts_lang, query_src) {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("FAIL {:?}: {}", lang, e);
                    failures += 1;
                }
            }
        }
        eprintln!(
            "query compile check: {} checked, {} skipped, {} failed",
            languages.len() - skipped,
            skipped,
            failures
        );
        assert_eq!(failures, 0, "{} query files failed to compile", failures);
    }
