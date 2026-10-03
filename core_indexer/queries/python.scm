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

; Any receiver: `m.f()`, `self.a.f()`, `mod.sub.f()`, `g().f()`.
(call
  function: (attribute
    attribute: (identifier) @call.method)) @call

;; ── Imports ─────────────────────────────────────────────────────────

(import_statement) @import

(import_from_statement) @import

;; ── Decorators ──────────────────────────────────────────────────────

(decorator) @decorator

;; ── Returns (return-type evidence) ──────────────────────────────────

(return_statement (call)) @return.stmt

;; ── Assignments ─────────────────────────────────────────────────────

(assignment
  left: (identifier) @field.name) @field

; `self.x = ...` — instance attributes; only used as type evidence.
(assignment
  left: (attribute) @field.attr) @field

;; ── Docstrings ──────────────────────────────────────────────────────

(expression_statement (string) @docstring)
