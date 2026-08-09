; CodeRadar v3.5 — Swift tree-sitter queries (§4.2)
; tree-sitter-swift via tree-sitter-language-pack

;; ── Classes / Structs / Enums / Protocols ────────────────────────────

(class_declaration) @class.def
(struct_declaration) @class.def
(enum_declaration) @class.def
(protocol_declaration) @class.def
(extension_declaration) @class.def

;; ── Functions / Methods ─────────────────────────────────────────────

(function_declaration) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(call_expression) @call

;; ── Imports ─────────────────────────────────────────────────────────

(import_declaration) @import

;; ── Doc comments ────────────────────────────────────────────────────

(doc_comment) @docstring
