; CodeRadar v3.5 — Elixir tree-sitter queries (§4.2)
; tree-sitter-elixir via tree-sitter-language-pack
;
; Elixir grammar represents both `defmodule` and `def`/`defp` as
; (call target: (identifier) ...) nodes — identical structure.
; We use #match? predicates to distinguish them.
;
; IMPORTANT: Function patterns MUST come before Class patterns.
; When tree-sitter predicates fail to filter (known issue with some
; grammars), single-pass extraction uses first-capture-wins dedup.
; A `def greet` inside a module must dispatch as Function, not Class.

;; ── Functions ───────────────────────────────────────────────────────
;; def foo do ... end / defp foo do ... end

(call
  target: (identifier) @_def_target
  (do_block) @_body) @function.def
(#match? @_def_target "^(def|defp)$")

;; ── Modules ─────────────────────────────────────────────────────────
;; defmodule Foo do ... end

(call
  target: (identifier) @_mod_target
  (do_block) @_body) @class.def
(#match? @_mod_target "^(defmodule)$")

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
