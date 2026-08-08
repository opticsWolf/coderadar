; CodeRadar v3.3 — TypeScript / JavaScript tree-sitter queries (§4.2)

;; ── Classes ─────────────────────────────────────────────────────────

(class_declaration
  name: (type_identifier) @class.name) @class.def

(abstract_class_declaration
  name: (type_identifier) @class.name) @class.def

(class_declaration
  extends: (type_annotation
    (type_identifier) @class_base)*)

;; ── Functions ───────────────────────────────────────────────────────

(function_declaration
  name: (identifier) @function.name) @function.def

(method_definition
  name: (property_identifier) @function.name) @function.def

(arrow_function) @function.arrow

(generator_function_declaration
  name: (identifier) @function.name) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(call_expression
  function: (identifier) @call.name) @call

(call_expression
  function: (member_expression
    object: (identifier) @call.receiver
    property: (property_identifier) @call.method)) @call

;; ── Imports ─────────────────────────────────────────────────────────

(import_statement
  source: (string) @import.module) @import

(import_statement
  import_clause
    name: (identifier) @import.name) @import

(lexical_declaration
  (variable_declarator
    name: (identifier) @import.name
    value: (call_expression
      function: (identifier) @import.require)))

;; ── Decorators ──────────────────────────────────────────────────────

(decorator
  (identifier) @decorator.name) @decorator

(decorator
  (call_expression
    function: (identifier) @decorator.name)) @decorator

;; ── Exports ─────────────────────────────────────────────────────────

(export_statement
  declaration: (function_declaration
    name: (identifier) @export.function)) @export

(export_statement
  declaration: (class_declaration
    name: (type_identifier) @export.class)) @export

(export_statement
  source: (string) @export.module) @export

;; ── Fields / Properties ─────────────────────────────────────────────

(public_field_definition
  name: (property_identifier) @field.name) @field

(class_declaration
  body: (class_body
    (public_field_definition
      name: (property_identifier) @field.name))) @field

;; ── Docstrings / JSDoc ──────────────────────────────────────────────

(comment) @docstring
