; CodeRadar v3.6 — GraphQL tree-sitter queries
; tree-sitter-graphql via tree-sitter-language-pack

;; ── Object / Input / Enum types ─────────────────────────────────────

(object_type_definition) @class.def
(input_object_type_definition) @class.def
(enum_type_definition) @class.def

;; ── Field definitions ───────────────────────────────────────────────

(field_definition) @field

;; ── Queries / Mutations (call-like) ─────────────────────────────────

(operation_definition) @call

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
