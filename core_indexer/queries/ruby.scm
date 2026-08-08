; CodeRadar v3.5 — Ruby tree-sitter queries (§4.2)
; Compatible with tree-sitter-ruby 0.23.x

;; ── Classes / Modules ────────────────────────────────────────────────

(class) @class.def
(module) @class.def

;; ── Methods / Functions ─────────────────────────────────────────────

(method) @function.def
(singleton_method) @function.def

;; ── Calls ───────────────────────────────────────────────────────────

(call) @call

;; ── Comments ────────────────────────────────────────────────────────

(comment) @docstring
