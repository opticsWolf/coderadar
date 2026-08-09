; CodeRadar v3.5 — Zig tree-sitter queries (§4.2)
; tree-sitter-zig via tree-sitter-language-pack

;; ── Structs / Enums / Unions ────────────────────────────────────────

(struct_declaration) @class.def
(enum_declaration) @class.def
(union_declaration) @class.def
(opaque_declaration) @class.def

;; ── Functions ───────────────────────────────────────────────────────

(function_declaration) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(call_expression) @call

;; ── Imports ─────────────────────────────────────────────────────────

(use_declaration) @import

;; ── Doc comments ────────────────────────────────────────────────────

(doc_comment) @docstring
