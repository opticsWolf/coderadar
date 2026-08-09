; CodeRadar v3.5 — Scala tree-sitter queries (§4.2)
; tree-sitter-scala via tree-sitter-language-pack

;; ── Classes / Objects / Traits ──────────────────────────────────────

(class_definition) @class.def
(object_definition) @class.def
(trait_definition) @class.def

;; ── Functions / Methods ─────────────────────────────────────────────

(function_definition) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(call_expression) @call

;; ── Imports ─────────────────────────────────────────────────────────

(import_declaration) @import

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
