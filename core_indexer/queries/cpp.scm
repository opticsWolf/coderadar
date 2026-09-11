; CodeRadar v3.5 — C++ tree-sitter queries (§4.2)
; Compatible with tree-sitter-cpp 0.23.x

;; ── Classes / Structs ───────────────────────────────────────────────

(class_specifier) @class.def
(struct_specifier) @class.def

;; ── Functions ──────────────────────────────────────────────────────

(function_definition) @function.def

;; ── Calls ──────────────────────────────────────────────────────────

(call_expression) @call

;; R2-3: constructor calls (`new Store()`) -- invisible before.
(new_expression
  type: (type_identifier) @call.name) @call

;; ── Includes ───────────────────────────────────────────────────────

(preproc_include) @import

;; ── Comments ───────────────────────────────────────────────────────

(comment) @docstring
