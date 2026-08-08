; CodeRadar v3.5 — Go tree-sitter queries (§4.2)
; Compatible with tree-sitter-go 0.23.x

;; ── Structs / Interfaces ─────────────────────────────────────────────

(type_declaration) @class.def

;; ── Functions ──────────────────────────────────────────────────────

(function_declaration
  name: (identifier) @function.name) @function.def

(method_declaration
  name: (field_identifier) @function.name) @function.def

;; ── Calls ──────────────────────────────────────────────────────────

(call_expression
  function: (identifier) @call.name) @call

(call_expression
  function: (selector_expression
    operand: (identifier) @call.receiver
    field: (field_identifier) @call.method)) @call

;; ── Imports ────────────────────────────────────────────────────────

(import_declaration) @import

;; ── Comments ───────────────────────────────────────────────────────

(comment) @docstring
