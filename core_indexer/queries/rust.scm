; CodeRadar v3.5 — Rust tree-sitter queries (§4.2)
; Compatible with tree-sitter-rust 0.24.2

;; ── Structs / Enums / Traits / Unions ────────────────────────────────

(struct_item
  name: (type_identifier) @class.name) @class.def

(enum_item
  name: (type_identifier) @class.name) @class.def

(trait_item
  name: (type_identifier) @class.name) @class.def

(union_item
  name: (type_identifier) @class.name) @class.def

(type_item
  name: (type_identifier) @class.name) @class.def

;; ── Functions ──────────────────────────────────────────────────────

(function_item
  name: (identifier) @function.name) @function.def

;; ── Impl blocks — mark methods inside as belonging to the type ──────

(impl_item) @impl

;; ── Calls ──────────────────────────────────────────────────────────

(call_expression
  function: (identifier) @call.name) @call

(call_expression
  function: (field_expression
    value: (self) @call.receiver
    field: (field_identifier) @call.method)) @call

(call_expression
  function: (scoped_identifier
    name: (identifier) @call.method)) @call

;; ── Macro invocations as calls ─────────────────────────────────────

(macro_invocation
  macro: (identifier) @call.name) @call

;; ── Imports ────────────────────────────────────────────────────────

(use_declaration) @import

;; ── Doc comments ───────────────────────────────────────────────────

(line_comment) @docstring
(block_comment) @docstring
