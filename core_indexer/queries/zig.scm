; CodeRadar v3.6 — Zig tree-sitter queries
; tree-sitter-zig via tree-sitter-language-pack
; Node kinds verified via AST dump:
;   FnProto (function declarations, field: "function"),
;   VarDecl (variable declarations, field: "variable_type_function"),
;   ContainerDecl (struct/enum/union bodies, inside VarDecl),
;   FnCallArguments (call argument lists),
;   BUILTINIDENTIFIER (@import, @cImport, etc.),
;   IDENTIFIER, BuildinTypeExpr (i32, f64, void, etc.)

;; ── Functions ───────────────────────────────────────────────────────
;; name extracted from function: (IDENTIFIER) field

(FnProto) @function.def

;; ── Structs / Enums / Unions ────────────────────────────────────────
;; All use VarDecl wrapping ContainerDecl

(VarDecl) @class.def

;; ── Calls ───────────────────────────────────────────────────────────
;; Calls appear as SuffixExpr with FnCallArguments child

(FnCallArguments) @call

;; ── Imports ─────────────────────────────────────────────────────────
;; @import("std") → captured as BUILTINIDENTIFIER

(BUILTINIDENTIFIER) @import

;; ── Comments ────────────────────────────────────────────────────────

;; Note: Zig grammar does not have a "comment" node kind.
;; Docstring extraction uses the preceding-comment scanner instead.
