; CodeRadar v3.3 — Python tree-sitter queries (§4.2)
; Standard capture names for the tagger pass.

;; ── Classes ─────────────────────────────────────────────────────────

(class_definition
  name: (identifier) @class.name) @class.def

(class_definition
  body: (block
    (expression_statement
      (string) @docstring)?))

(class_definition
  bases: (argument_list
    (identifier) @class_base)*)

;; ── Functions ───────────────────────────────────────────────────────

(function_definition
  name: (identifier) @function.name) @function.def

(async_function_definition
  name: (identifier) @function.name) @function.def

(function_definition
  parameters: (parameters) @function.params)

(function_definition
  return_type: (type)? @function.return)

;; ── Calls ───────────────────────────────────────────────────────────

(call
  function: (identifier) @call.name) @call

(call
  function: (attribute
    object: (identifier) @call.receiver
    attribute: (identifier) @call.method)) @call

(call
  arguments: (argument_list) @call.args)

;; ── Imports ─────────────────────────────────────────────────────────

(import_statement
  name: (dotted_name) @import.module) @import

(import_from_statement
  module_name: (dotted_name) @import_from.module
  name: (dotted_name) @import_from.name) @import_from

(import_from_statement
  module_name: (dotted_name) @import_from.module
  name: (aliased_import
    name: (dotted_name) @import_from.name
    alias: (identifier) @import_from.alias))

;; ── Decorators ──────────────────────────────────────────────────────

(decorator
  (identifier) @decorator.name) @decorator

(decorator
  (attribute
    attribute: (identifier) @decorator.name)) @decorator

(decorator
  (call
    function: (identifier) @decorator.name)) @decorator

;; ── Fields / Assignments ────────────────────────────────────────────

(module
  (expression_statement
    (assignment
      left: (identifier) @field.name))) @field

(class_definition
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @field.name)))) @field

;; ── Docstrings ──────────────────────────────────────────────────────

(module
  . (expression_statement (string) @docstring))

(function_definition
  body: (block . (expression_statement (string) @docstring)))

(class_definition
  body: (block . (expression_statement (string) @docstring)))
