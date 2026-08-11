; CodeRadar v3.6 — Dart tree-sitter queries
; tree-sitter-dart via tree-sitter-language-pack

;; ── Classes ─────────────────────────────────────────────────────────

(class_definition) @class.def

;; ── Functions / Methods ─────────────────────────────────────────────

(function_signature) @function.def

;; ── Imports ─────────────────────────────────────────────────────────

(import_specification) @import.module

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
