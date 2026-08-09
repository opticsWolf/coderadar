; CodeRadar v3.5 — Elixir tree-sitter queries (§4.2)
; tree-sitter-elixir via tree-sitter-language-pack
;
; Elixir grammar represents both `defmodule` and `def`/`defp` as
; (call target: (identifier) ...) nodes — identical structure.
; We use #match? predicates to distinguish them.

;; ── Modules ─────────────────────────────────────────────────────────
;; defmodule Foo do ... end

(call
  target: (identifier) @_mod_target
  (do_block) @_body) @class.def
(#match? @_mod_target "^(defmodule)$")

;; ── Functions ───────────────────────────────────────────────────────
;; def foo do ... end / defp foo do ... end
;; Based on CodeGraph's classifyFunctionNode pattern (swift.rs, kotlin.rs)
;; Copyright (c) 2024 Colby McHenry — MIT License
;; <https://github.com/colbymchenry/codegraph>

(call
  target: (identifier) @_def_target
  (do_block) @_body) @function.def
(#match? @_def_target "^(def|defp)$")

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
