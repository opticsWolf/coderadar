; CodeRadar v3.5 — TypeScript / JavaScript tree-sitter queries (§4.2)
; Compatible with tree-sitter-typescript 0.23.x

;; ── Classes ─────────────────────────────────────────────────────────

(class_declaration
  name: (type_identifier) @class.name) @class.def

(abstract_class_declaration
  name: (type_identifier) @class.name) @class.def

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

(public_field_definition
  name: (property_identifier) @field.name) @field

;; ── Docstrings / JSDoc ──────────────────────────────────────────────

(comment) @docstring
