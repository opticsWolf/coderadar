; CodeRadar v3.5 — Java tree-sitter queries (§4.2)
; Compatible with tree-sitter-java 0.23.x

;; ── Classes / Interfaces ────────────────────────────────────────────

(class_declaration
  name: (identifier) @class.name) @class.def

(interface_declaration
  name: (identifier) @class.name) @class.def

;; ── Methods / Functions ────────────────────────────────────────────

(method_declaration
  name: (identifier) @function.name) @function.def

(constructor_declaration
  name: (identifier) @function.name) @function.def

;; ── Calls ──────────────────────────────────────────────────────────

(method_invocation
  name: (identifier) @call.name) @call

(method_invocation
  object: (identifier) @call.receiver
  name: (identifier) @call.method) @call

;; R2-3: constructor calls (`new Store()`) -- invisible before.
;; (Scoped `new pkg.Store()` stays uncovered: the type node is a
;; scoped_type_identifier, not a plain type_identifier.)
(object_creation_expression
  type: (type_identifier) @call.name) @call

;; ── Imports ────────────────────────────────────────────────────────

(import_declaration) @import

;; ── Comments ───────────────────────────────────────────────────────

(block_comment) @docstring
(line_comment) @docstring
