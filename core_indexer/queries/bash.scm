; CodeRadar v3.6 — Bash tree-sitter queries
; tree-sitter-bash via tree-sitter-language-pack

;; ── Functions ───────────────────────────────────────────────────────

(function_definition) @function.def

;; ── Calls (command invocations) ─────────────────────────────────────

(command) @call

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
