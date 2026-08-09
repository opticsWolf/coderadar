; CodeRadar v3.5 — Kotlin tree-sitter queries (§4.2)
; Compatible with tree-sitter-kotlin (via tree-sitter-language-pack)

;; ── Classes ──────────────────────────────────────────────────────────

(class_declaration) @class.def
(object_declaration) @class.def

;; ── Functions / Methods ─────────────────────────────────────────────

(function_declaration) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(call_expression) @call

;; ── Imports ─────────────────────────────────────────────────────────

(import_header) @import

;; ── Line comments ───────────────────────────────────────────────────

(line_comment) @docstring
