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

;; ── Imports ────────────────────────────────────────────────────────

(import_declaration) @import

;; ── Comments ───────────────────────────────────────────────────────

(block_comment) @docstring
(line_comment) @docstring
