; CodeRadar v3.5 — Python tree-sitter queries (§4.2)
; Compatible with tree-sitter-python 0.23.6
; Minimal patterns verified against the grammar.

;; ── Classes ─────────────────────────────────────────────────────────

(class_definition
  name: (identifier) @class.name) @class.def

;; ── Functions ───────────────────────────────────────────────────────

(function_definition
  name: (identifier) @function.name) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(call
  function: (identifier) @call.name) @call

(call
  function: (attribute
    object: (identifier) @call.receiver
    attribute: (identifier) @call.method)) @call

;; ── Imports ─────────────────────────────────────────────────────────

(import_statement) @import

(import_from_statement) @import

;; ── Decorators ──────────────────────────────────────────────────────

(decorator) @decorator

;; ── Assignments ─────────────────────────────────────────────────────

(assignment
  left: (identifier) @field.name) @field

;; ── Docstrings ──────────────────────────────────────────────────────

(expression_statement (string) @docstring)
