; CodeRadar v3.5 — JavaScript tree-sitter queries (§4.2)
; Compatible with tree-sitter-javascript 0.23.x
; Note: JS uses (identifier) for class names, not (type_identifier) like TS.

;; ── Classes ─────────────────────────────────────────────────────────

(class_declaration
  name: (identifier) @class.name) @class.def

;; ── Functions ───────────────────────────────────────────────────────

(function_declaration
  name: (identifier) @function.name) @function.def

(method_definition
  name: (property_identifier) @function.name) @function.def

(generator_function_declaration
  name: (identifier) @function.name) @function.def

(arrow_function) @function.arrow

;; ── Calls ───────────────────────────────────────────────────────────

(call_expression
  function: (identifier) @call.name) @call

(call_expression
  function: (member_expression
    object: (identifier) @call.receiver
    property: (property_identifier) @call.method)) @call

;; ── Imports ─────────────────────────────────────────────────────────

(import_statement) @import

;; ── Decorators ──────────────────────────────────────────────────────

(decorator) @decorator

;; ── Exports ─────────────────────────────────────────────────────────

(export_statement) @export

;; ── Fields / Properties ─────────────────────────────────────────────
; Note: JS tree-sitter doesn't have field_definition; field queries are TS-only.

;; ── Docstrings / JSDoc ──────────────────────────────────────────────

(comment) @docstring
