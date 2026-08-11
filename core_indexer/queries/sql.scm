; CodeRadar v3.6 — SQL tree-sitter queries
; tree-sitter-sql via tree-sitter-language-pack

;; ── CREATE TABLE (class-like) ───────────────────────────────────────

(create_table) @class.def

;; ── Column definitions ──────────────────────────────────────────────

(column_definition) @field

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
