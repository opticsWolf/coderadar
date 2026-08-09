; CodeRadar v3.6 — R tree-sitter queries
; tree-sitter-r via tree-sitter-language-pack
; Node kinds verified via AST dump:
;   function_definition (parameters: field, no name: field),
;   binary_operator (lhs: identifier <- rhs: function_definition),
;   call (function: field), identifier, braced_expression,
;   comment, string
; Note: R function names come from the parent binary_operator's
; lhs: field (e.g. greet <- function(name) { ... }), not from
; the function_definition node itself. The walker handles this.

;; ── Functions ───────────────────────────────────────────────────────

(function_definition) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(call) @call

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
