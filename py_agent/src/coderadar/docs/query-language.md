# Query language reference

Generated from the query grammar and field schema — do not edit by
hand. `CodeRadar` ships this as the single source of truth: the parser
rejects unknown fields with the same list shown here.

## Query shape

```
<entity> [select <field>[, <field>] | <agg>(<field>) as <alias>]
[where <predicate>] [group by <field>] [order by <field> [asc|desc]]
[limit <n>]
```

Operators: `==`, `!=`, `<`, `<=`, `>`, `>=`, `contains`, `matches`
(regex), `starts_with`, `ends_with`, `in`. Predicates combine with
`and`, `or`, `not`; `not` binds tightest, `or` loosest.

Every row also carries the identity fields `id`, `file_path`, `kind`, `parent_id`, whatever `select` says.

## Examples

```
classes where inherits_from contains "BaseModel"
methods where is_async == true
functions where line_count > 50 order by line_count desc limit 10
functions where caller_count == 0 and not name matches "^test_"
functions where name starts_with "test_"
functions where decorators contains "deprecated"
functions where kind == "property"
functions where is_override == true
classes where is_abstract == true
classes where has_method("__init__") == true and has_method("__eq__") == false
classes select is_abstract, count(*) as n group by is_abstract order by n desc limit 20
constants where name == "VERSION"
imports where import_kind == "from"
calls where target_kind == "external"
entities where name contains "Session"
modules where path ends_with "app.py"
```

## `modules`

| Field | Type | Meaning |
|---|---|---|
| `id` | str | Entity id — the key every other API takes |
| `file_path` | str | Path of the file the entity lives in |
| `kind` | str | Entity kind (module, class, constant, import, call, field) — functions report their flavour (method, static, property, …) |
| `parent_id` | str | Owning entity id (class for methods/fields, module for classes), or null |
| `name` | str | Module name |
| `path` | str | File path on disk |
| `language` | str | Source language |
| `class_count` | int | Classes defined in the module |
| `function_count` | int | Functions defined in the module |
| `import_count` | int | Imports in the module |

## `classes`

| Field | Type | Meaning |
|---|---|---|
| `id` | str | Entity id — the key every other API takes |
| `file_path` | str | Path of the file the entity lives in |
| `kind` | str | Entity kind (module, class, constant, import, call, field) — functions report their flavour (method, static, property, …) |
| `parent_id` | str | Owning entity id (class for methods/fields, module for classes), or null |
| `name` | str | Class name |
| `line` | int | Definition line |
| `method_count` | int | Methods defined on the class |
| `decorators` | list[str] | Decorator texts, e.g. `@dataclass` |
| `bases` | list[str] | Base class names as written in source |
| `base_ids` | list[str] | Resolved in-repo base class ids |
| `inherits_from` | list[str] | Transitive ancestors from the MRO (the class itself excluded) — in-repo ids and external base names |
| `is_abstract` | bool | ABC/Protocol class, or defines at least one abstract method |
| `docstring` | str | Class docstring (empty when absent) |

## `functions`

| Field | Type | Meaning |
|---|---|---|
| `id` | str | Entity id — the key every other API takes |
| `file_path` | str | Path of the file the entity lives in |
| `kind` | str | Entity kind (module, class, constant, import, call, field) — functions report their flavour (method, static, property, …) |
| `parent_id` | str | Owning entity id (class for methods/fields, module for classes), or null |
| `name` | str | Function or method name |
| `line` | int | Definition line |
| `line_count` | int | Definition span in lines |
| `parent_class` | str | Owning class id for methods (empty for free functions) |
| `is_async` | bool | `async def` |
| `is_override` | bool | Overrides a base-class method |
| `complexity` | int | McCabe cyclomatic complexity |
| `decorators` | list[str] | Decorator texts |
| `parameter_count` | int | Declared parameters |
| `return_type` | str | Return annotation (absent when unannotated) |
| `docstring` | str | Function docstring (empty when absent) |
| `caller_count` | int | Number of distinct callers |
| `callee_count` | int | Number of distinct callees |
| `callers` | list[str] | Caller entity ids |
| `callees` | list[str] | Callee entity ids |
| `resolved_call_targets` | list[str] | Every resolved call target id, including external/builtin names |

## `methods`

| Field | Type | Meaning |
|---|---|---|
| `id` | str | Entity id — the key every other API takes |
| `file_path` | str | Path of the file the entity lives in |
| `kind` | str | Entity kind (module, class, constant, import, call, field) — functions report their flavour (method, static, property, …) |
| `parent_id` | str | Owning entity id (class for methods/fields, module for classes), or null |
| `name` | str | Function or method name |
| `line` | int | Definition line |
| `line_count` | int | Definition span in lines |
| `parent_class` | str | Owning class id for methods (empty for free functions) |
| `is_async` | bool | `async def` |
| `is_override` | bool | Overrides a base-class method |
| `complexity` | int | McCabe cyclomatic complexity |
| `decorators` | list[str] | Decorator texts |
| `parameter_count` | int | Declared parameters |
| `return_type` | str | Return annotation (absent when unannotated) |
| `docstring` | str | Function docstring (empty when absent) |
| `caller_count` | int | Number of distinct callers |
| `callee_count` | int | Number of distinct callees |
| `callers` | list[str] | Caller entity ids |
| `callees` | list[str] | Callee entity ids |
| `resolved_call_targets` | list[str] | Every resolved call target id, including external/builtin names |

## `constants`

| Field | Type | Meaning |
|---|---|---|
| `id` | str | Entity id — the key every other API takes |
| `file_path` | str | Path of the file the entity lives in |
| `kind` | str | Entity kind (module, class, constant, import, call, field) — functions report their flavour (method, static, property, …) |
| `parent_id` | str | Owning entity id (class for methods/fields, module for classes), or null |
| `name` | str | Constant name |
| `value` | str | Assigned value text |
| `annotation` | str | Type annotation text (empty when absent) |

## `entities`

| Field | Type | Meaning |
|---|---|---|
| `id` | str | Entity id — the key every other API takes |
| `file_path` | str | Path of the file the entity lives in |
| `kind` | str | Entity kind (module, class, constant, import, call, field) — functions report their flavour (method, static, property, …) |
| `parent_id` | str | Owning entity id (class for methods/fields, module for classes), or null |
| `name` | str | Module name |
| `path` | str | File path on disk |
| `language` | str | Source language |
| `class_count` | int | Classes defined in the module |
| `function_count` | int | Functions defined in the module |
| `import_count` | int | Imports in the module |
| `line` | int | Definition line |
| `method_count` | int | Methods defined on the class |
| `decorators` | list[str] | Decorator texts, e.g. `@dataclass` |
| `bases` | list[str] | Base class names as written in source |
| `base_ids` | list[str] | Resolved in-repo base class ids |
| `inherits_from` | list[str] | Transitive ancestors from the MRO (the class itself excluded) — in-repo ids and external base names |
| `is_abstract` | bool | ABC/Protocol class, or defines at least one abstract method |
| `docstring` | str | Class docstring (empty when absent) |
| `line_count` | int | Definition span in lines |
| `parent_class` | str | Owning class id for methods (empty for free functions) |
| `is_async` | bool | `async def` |
| `is_override` | bool | Overrides a base-class method |
| `complexity` | int | McCabe cyclomatic complexity |
| `parameter_count` | int | Declared parameters |
| `return_type` | str | Return annotation (absent when unannotated) |
| `caller_count` | int | Number of distinct callers |
| `callee_count` | int | Number of distinct callees |
| `callers` | list[str] | Caller entity ids |
| `callees` | list[str] | Callee entity ids |
| `resolved_call_targets` | list[str] | Every resolved call target id, including external/builtin names |
| `value` | str | Assigned value text |
| `annotation` | str | Type annotation text (empty when absent) |

## `imports`

| Field | Type | Meaning |
|---|---|---|
| `id` | str | Entity id — the key every other API takes |
| `file_path` | str | Path of the file the entity lives in |
| `kind` | str | Entity kind (module, class, constant, import, call, field) — functions report their flavour (method, static, property, …) |
| `parent_id` | str | Owning entity id (class for methods/fields, module for classes), or null |
| `raw` | str | Import statement text |
| `import_kind` | str | module, from, relative, star or side |
| `line` | int | Statement line |
| `is_type_only` | bool | TYPE_CHECKING-only import |
| `resolved_module` | str | Resolved module id (module and wildcard imports) |
| `resolved_target` | str | Resolved symbol id (from-imports) |
| `target_kind` | str | module, class, function, import, wildcard, external, dynamic, unresolved |

## `calls`

| Field | Type | Meaning |
|---|---|---|
| `id` | str | Entity id — the key every other API takes |
| `file_path` | str | Path of the file the entity lives in |
| `kind` | str | Entity kind (module, class, constant, import, call, field) — functions report their flavour (method, static, property, …) |
| `parent_id` | str | Owning entity id (class for methods/fields, module for classes), or null |
| `source` | str | Caller entity id |
| `target` | str | Callee entity id |
| `target_kind` | str | function, class or external |

## `fields`

| Field | Type | Meaning |
|---|---|---|
| `id` | str | Entity id — the key every other API takes |
| `file_path` | str | Path of the file the entity lives in |
| `kind` | str | Entity kind (module, class, constant, import, call, field) — functions report their flavour (method, static, property, …) |
| `parent_id` | str | Owning entity id (class for methods/fields, module for classes), or null |
| `name` | str | Field name |
| `parent_class` | str | Owning class id |
| `type_annotation` | str | Annotation text (absent when unannotated) |
| `is_class_var` | bool | Class-level (rather than instance) attribute |

