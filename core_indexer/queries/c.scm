; CodeRadar v3.5 — C tree-sitter queries (§4.2)
; Compatible with tree-sitter-c 0.23.x

;; ── Structs ────────────────────────────────────────────────────────

(struct_specifier) @class.def

;; ── Functions ──────────────────────────────────────────────────────

(function_definition) @function.def

;; ── Calls ──────────────────────────────────────────────────────────

(call_expression) @call

;; ── Includes ───────────────────────────────────────────────────────

(preproc_include) @import

;; ── Comments ───────────────────────────────────────────────────────

(comment) @docstring
