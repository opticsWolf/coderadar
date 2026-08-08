; CodeRadar v3.5 — PHP tree-sitter queries (§4.2)
; Compatible with tree-sitter-php 0.24.x

;; ── Classes / Interfaces / Traits ────────────────────────────────────

(class_declaration) @class.def
(interface_declaration) @class.def
(trait_declaration) @class.def

;; ── Functions / Methods ──────────────────────────────────────────────

(function_definition) @function.def
(method_declaration) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(function_call_expression) @call
(member_call_expression) @call

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
