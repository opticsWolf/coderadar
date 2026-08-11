; CodeRadar v3.6 — Dockerfile tree-sitter queries
; tree-sitter-dockerfile via tree-sitter-language-pack

;; ── FROM instruction (stage definitions) ───────────────────────────

(from_instruction) @class.def

;; ── COPY / ADD ─────────────────────────────────────────────────────

(copy_instruction) @call

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
