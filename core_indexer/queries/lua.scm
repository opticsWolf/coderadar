; CodeRadar v3.5 — Lua tree-sitter queries (§4.2)
; tree-sitter-lua via tree-sitter-language-pack

;; ── Functions ───────────────────────────────────────────────────────

(function_declaration) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(function_call) @call

;; ── Tables (Lua's classes/modules) ──────────────────────────────────

(table_constructor) @class.def

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
