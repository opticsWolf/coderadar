; CodeRadar v3.5 — C# tree-sitter queries (§4.2)
; Compatible with tree-sitter-c-sharp 0.23.x

;; ── Classes / Interfaces / Structs ───────────────────────────────────

(class_declaration) @class.def
(interface_declaration) @class.def
(struct_declaration) @class.def

;; ── Methods / Functions ─────────────────────────────────────────────

(method_declaration) @function.def
(local_function_statement) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(invocation_expression) @call

;; R2-3: constructor calls (`new Store()`) -- invisible before.
(object_creation_expression
  type: (identifier) @call.name) @call

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
