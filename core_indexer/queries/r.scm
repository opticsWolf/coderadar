; CodeRadar v3.5 — R tree-sitter queries (§4.2)
; tree-sitter-r via tree-sitter-language-pack

;; ── Functions ───────────────────────────────────────────────────────

(function_definition
  name: (identifier) @name) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(call) @call

;; ── S4 classes ──────────────────────────────────────────────────────

(setClass_expression) @class.def

;; ── Library imports ─────────────────────────────────────────────────

(library_call) @import

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
