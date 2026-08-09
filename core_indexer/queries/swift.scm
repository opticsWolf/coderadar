; CodeRadar v3.6 — Swift tree-sitter queries
; tree-sitter-swift via tree-sitter-language-pack
; Node kinds verified via AST dump:
;   class_declaration, function_declaration, call_expression,
;   import_declaration, simple_identifier, type_identifier
; Swift uses class_declaration for class/struct/enum/protocol/extension;
; disambiguation happens in classify_class_like() in walker.rs.

;; ── Classes / Structs / Enums / Protocols ────────────────────────────
;; Swift grammar uses a single class_declaration node kind for
;; class, struct, enum, protocol, extension, actor.

(class_declaration) @class.def

;; ── Functions / Methods ─────────────────────────────────────────────

(function_declaration) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(call_expression) @call

;; ── Imports ─────────────────────────────────────────────────────────

(import_declaration) @import

;; ── Comments (for docstring extraction) ─────────────────────────────

(comment) @docstring
