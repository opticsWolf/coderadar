// CodeRadar v0.10 — Query: static field schema (precision plan §3.2 / §3.5)
//
// Unknown fields used to evaluate to `Null`, so `functions where bogus == 1`
// returned zero rows — indistinguishable from "no matches" — and typos like
// `file_path` on `functions` read as empty results. Every field a query may
// name is declared here once; the parser rejects anything else with the
// available list, and this same table generates the documentation.

use crate::query::grammar::{EntityType, Operand, ParsedQuery, Predicate, SelectItem};

/// One queryable field of an entity kind.
#[derive(Clone, Copy, Debug)]
pub struct FieldSpec {
    pub name: &'static str,
    /// Short type label for docs: `str`, `int`, `bool`, `list[str]`.
    pub ty: &'static str,
    pub doc: &'static str,
}

/// Row-identity fields, present on EVERY row regardless of `select` (§3.3).
pub const IDENTITY_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        ty: "str",
        doc: "Entity id — the key every other API takes",
    },
    FieldSpec {
        name: "file_path",
        ty: "str",
        doc: "Path of the file the entity lives in",
    },
    FieldSpec {
        name: "kind",
        ty: "str",
        doc: "Entity kind (module, class, constant, import, call, field) — functions report their flavour (method, static, property, …)",
    },
    FieldSpec {
        name: "parent_id",
        ty: "str",
        doc: "Owning entity id (class for methods/fields, module for classes), or null",
    },
];

pub const MODULE_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "name",
        ty: "str",
        doc: "Module name",
    },
    FieldSpec {
        name: "path",
        ty: "str",
        doc: "File path on disk",
    },
    FieldSpec {
        name: "language",
        ty: "str",
        doc: "Source language",
    },
    FieldSpec {
        name: "class_count",
        ty: "int",
        doc: "Classes defined in the module",
    },
    FieldSpec {
        name: "function_count",
        ty: "int",
        doc: "Functions defined in the module",
    },
    FieldSpec {
        name: "import_count",
        ty: "int",
        doc: "Imports in the module",
    },
];

pub const CLASS_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "name",
        ty: "str",
        doc: "Class name",
    },
    FieldSpec {
        name: "line",
        ty: "int",
        doc: "Definition line",
    },
    FieldSpec {
        name: "method_count",
        ty: "int",
        doc: "Methods defined on the class",
    },
    FieldSpec {
        name: "decorators",
        ty: "list[str]",
        doc: "Decorator texts, e.g. `@dataclass`",
    },
    FieldSpec {
        name: "bases",
        ty: "list[str]",
        doc: "Base class names as written in source",
    },
    FieldSpec {
        name: "base_ids",
        ty: "list[str]",
        doc: "Resolved in-repo base class ids",
    },
    FieldSpec {
        name: "inherits_from",
        ty: "list[str]",
        doc: "Transitive ancestors from the MRO (the class itself excluded) — in-repo ids and external base names",
    },
    FieldSpec {
        name: "is_abstract",
        ty: "bool",
        doc: "ABC/Protocol class, or defines at least one abstract method",
    },
    FieldSpec {
        name: "docstring",
        ty: "str",
        doc: "Class docstring (empty when absent)",
    },
];

pub const FUNCTION_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "name",
        ty: "str",
        doc: "Function or method name",
    },
    FieldSpec {
        name: "line",
        ty: "int",
        doc: "Definition line",
    },
    FieldSpec {
        name: "line_count",
        ty: "int",
        doc: "Definition span in lines",
    },
    FieldSpec {
        name: "kind",
        ty: "str",
        doc: "Function flavour: method, function, static, classmethod, property, abstract, …",
    },
    FieldSpec {
        name: "parent_class",
        ty: "str",
        doc: "Owning class id for methods (empty for free functions)",
    },
    FieldSpec {
        name: "is_async",
        ty: "bool",
        doc: "`async def`",
    },
    FieldSpec {
        name: "is_override",
        ty: "bool",
        doc: "Overrides a base-class method",
    },
    FieldSpec {
        name: "complexity",
        ty: "int",
        doc: "McCabe cyclomatic complexity",
    },
    FieldSpec {
        name: "decorators",
        ty: "list[str]",
        doc: "Decorator texts",
    },
    FieldSpec {
        name: "parameter_count",
        ty: "int",
        doc: "Declared parameters",
    },
    FieldSpec {
        name: "return_type",
        ty: "str",
        doc: "Return annotation (absent when unannotated)",
    },
    FieldSpec {
        name: "docstring",
        ty: "str",
        doc: "Function docstring (empty when absent)",
    },
    FieldSpec {
        name: "caller_count",
        ty: "int",
        doc: "Number of distinct callers",
    },
    FieldSpec {
        name: "callee_count",
        ty: "int",
        doc: "Number of distinct callees",
    },
    FieldSpec {
        name: "callers",
        ty: "list[str]",
        doc: "Caller entity ids",
    },
    FieldSpec {
        name: "callees",
        ty: "list[str]",
        doc: "Callee entity ids",
    },
    FieldSpec {
        name: "resolved_call_targets",
        ty: "list[str]",
        doc: "Every resolved call target id, including external/builtin names",
    },
];

pub const CONSTANT_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "name",
        ty: "str",
        doc: "Constant name",
    },
    FieldSpec {
        name: "value",
        ty: "str",
        doc: "Assigned value text",
    },
    FieldSpec {
        name: "annotation",
        ty: "str",
        doc: "Type annotation text (empty when absent)",
    },
];

pub const IMPORT_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "raw",
        ty: "str",
        doc: "Import statement text",
    },
    FieldSpec {
        name: "import_kind",
        ty: "str",
        doc: "module, from, relative, star or side",
    },
    FieldSpec {
        name: "line",
        ty: "int",
        doc: "Statement line",
    },
    FieldSpec {
        name: "is_type_only",
        ty: "bool",
        doc: "TYPE_CHECKING-only import",
    },
    FieldSpec {
        name: "resolved_module",
        ty: "str",
        doc: "Resolved module id (module and wildcard imports)",
    },
    FieldSpec {
        name: "resolved_target",
        ty: "str",
        doc: "Resolved symbol id (from-imports)",
    },
    FieldSpec {
        name: "target_kind",
        ty: "str",
        doc: "module, class, function, import, wildcard, external, dynamic, unresolved",
    },
];

pub const CALL_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "source",
        ty: "str",
        doc: "Caller entity id",
    },
    FieldSpec {
        name: "target",
        ty: "str",
        doc: "Callee entity id",
    },
    FieldSpec {
        name: "target_kind",
        ty: "str",
        doc: "function, class or external",
    },
];

pub const FIELD_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "name",
        ty: "str",
        doc: "Field name",
    },
    FieldSpec {
        name: "parent_class",
        ty: "str",
        doc: "Owning class id",
    },
    FieldSpec {
        name: "type_annotation",
        ty: "str",
        doc: "Annotation text (absent when unannotated)",
    },
    FieldSpec {
        name: "is_class_var",
        ty: "bool",
        doc: "Class-level (rather than instance) attribute",
    },
];

/// Worked examples — rendered into `docs/query-language.md` and executed by
/// the Python suite, so a documented query that stops working fails CI.
pub const EXAMPLES: &[&str] = &[
    "classes where inherits_from contains \"BaseModel\"",
    "methods where is_async == true",
    "functions where line_count > 50 order by line_count desc limit 10",
    "functions where caller_count == 0 and not name matches \"^test_\"",
    "functions where name starts_with \"test_\"",
    "functions where decorators contains \"deprecated\"",
    "functions where kind == \"property\"",
    "functions where is_override == true",
    "classes where is_abstract == true",
    "classes where has_method(\"__init__\") == true and has_method(\"__eq__\") == false",
    "classes select is_abstract, count(*) as n group by is_abstract order by n desc limit 20",
    "constants where name == \"VERSION\"",
    "imports where import_kind == \"from\"",
    "calls where target_kind == \"external\"",
    "entities where name contains \"Session\"",
    "modules where path ends_with \"app.py\"",
];

/// The declared fields of an entity kind (identity fields included).
///
/// Identity comes first and wins a name collision: `functions` declares its
/// own `kind` (the method flavour) which is the same slot, documented once.
pub fn fields_for(entity: &EntityType) -> Vec<&'static FieldSpec> {
    let mut out: Vec<&'static FieldSpec> = IDENTITY_FIELDS.iter().collect();
    let tables: &[&[FieldSpec]] = match entity {
        EntityType::Modules => &[MODULE_FIELDS],
        EntityType::Classes => &[CLASS_FIELDS],
        EntityType::Functions | EntityType::Methods => &[FUNCTION_FIELDS],
        EntityType::Constants => &[CONSTANT_FIELDS],
        // Every name-bearing kind at once — the union of their fields.
        EntityType::Entities => &[
            MODULE_FIELDS,
            CLASS_FIELDS,
            FUNCTION_FIELDS,
            CONSTANT_FIELDS,
        ],
        EntityType::Imports => &[IMPORT_FIELDS],
        EntityType::Calls => &[CALL_FIELDS],
        EntityType::Fields => &[FIELD_FIELDS],
    };
    for table in tables {
        for spec in *table {
            if !out.iter().any(|s| s.name == spec.name) {
                out.push(spec);
            }
        }
    }
    out
}

pub fn field_names(entity: &EntityType) -> Vec<&'static str> {
    fields_for(entity).iter().map(|s| s.name).collect()
}

/// Validate every field a query names against the entity's schema.
///
/// Called from `parse_query`, so both the Rust and Python entry points fail
/// the same way. Derived-call arguments are exempt: their names are
/// implementation-defined lookups inside the call, not schema fields.
pub fn validate_query(query: &ParsedQuery) -> Result<(), String> {
    let names = field_names(&query.entity);
    // `count(*) as n ... order by n`: an alias is a name the query itself
    // defines, so it is legal wherever a field is.
    let aliases: Vec<&str> = query
        .select
        .iter()
        .filter_map(|item| match item {
            SelectItem::Aggregate { alias, .. } => Some(alias.as_str()),
            SelectItem::Path(_) => None,
        })
        .collect();
    let check = |field: &str| -> Result<(), String> {
        if names.contains(&field) || aliases.contains(&field) {
            Ok(())
        } else {
            Err(format!(
                "unknown field '{field}' for {}; available: {}",
                entity_name(&query.entity),
                names.join(", ")
            ))
        }
    };

    for item in &query.select {
        match item {
            SelectItem::Path(p) => check(p)?,
            SelectItem::Aggregate { path, .. } => {
                if path != "*" {
                    check(path)?;
                }
            }
        }
    }
    for path in &query.group_by {
        check(path)?;
    }
    if let Some(order) = &query.order_by {
        check(&order.path)?;
    }
    if let Some(pred) = &query.where_clause {
        validate_predicate(pred, &check)?;
    }
    Ok(())
}

fn validate_predicate(
    pred: &Predicate,
    check: &impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    match pred {
        Predicate::Comparison { left, op: _, right } => {
            validate_operand(left, check)?;
            validate_operand(right, check)
        }
        Predicate::Not(inner) => validate_predicate(inner, check),
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            validate_predicate(a, check)?;
            validate_predicate(b, check)
        }
    }
}

fn validate_operand(
    op: &Operand,
    check: &impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    match op {
        Operand::Path(parts) => check(&parts.join(".")),
        Operand::ListValue(items) => {
            for item in items {
                validate_operand(item, check)?;
            }
            Ok(())
        }
        // Derived-call arguments are exempt (see `validate_query`).
        Operand::DerivedCall { .. } => Ok(()),
        Operand::StringValue(_) | Operand::NumberValue(_) | Operand::BoolValue(_) => Ok(()),
    }
}

pub fn entity_name(entity: &EntityType) -> &'static str {
    match entity {
        EntityType::Modules => "modules",
        EntityType::Classes => "classes",
        EntityType::Functions => "functions",
        EntityType::Methods => "methods",
        EntityType::Constants => "constants",
        EntityType::Entities => "entities",
        EntityType::Imports => "imports",
        EntityType::Calls => "calls",
        EntityType::Fields => "fields",
    }
}

/// Markdown reference generated from this table (§3.5) — one section per
/// entity kind, identity fields first. `tests/test_query_language.py` asserts
/// `docs/query-language.md` matches this output, so the doc cannot drift.
pub fn render_markdown() -> String {
    let mut out = String::new();
    out.push_str("# Query language reference\n\n");
    out.push_str(
        "Generated from the query grammar and field schema — do not edit by\n\
         hand. `CodeRadar` ships this as the single source of truth: the parser\n\
         rejects unknown fields with the same list shown here.\n\n",
    );
    out.push_str("## Query shape\n\n");
    out.push_str(
        "```\n<entity> [select <field>[, <field>] | <agg>(<field>) as <alias>]\n\
         [where <predicate>] [group by <field>] [order by <field> [asc|desc]]\n\
         [limit <n>]\n```\n\n",
    );
    out.push_str(
        "Operators: `==`, `!=`, `<`, `<=`, `>`, `>=`, `contains`, `matches`\n\
         (regex), `starts_with`, `ends_with`, `in`. Predicates combine with\n\
         `and`, `or`, `not`; `not` binds tightest, `or` loosest.\n\n",
    );
    out.push_str("Every row also carries the identity fields ");
    out.push_str(
        &IDENTITY_FIELDS
            .iter()
            .map(|s| format!("`{}`", s.name))
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push_str(", whatever `select` says.\n\n");

    out.push_str("## Examples\n\n```\n");
    for example in EXAMPLES {
        out.push_str(example);
        out.push('\n');
    }
    out.push_str("```\n\n");

    let kinds: &[(EntityType, &str)] = &[
        (EntityType::Modules, "modules"),
        (EntityType::Classes, "classes"),
        (EntityType::Functions, "functions"),
        (EntityType::Methods, "methods"),
        (EntityType::Constants, "constants"),
        (EntityType::Entities, "entities"),
        (EntityType::Imports, "imports"),
        (EntityType::Calls, "calls"),
        (EntityType::Fields, "fields"),
    ];
    for (entity, label) in kinds {
        out.push_str(&format!("## `{label}`\n\n"));
        out.push_str("| Field | Type | Meaning |\n|---|---|---|\n");
        for spec in fields_for(entity) {
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                spec.name, spec.ty, spec.doc
            ));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::grammar::parse_query;

    #[test]
    fn keyword_prefixed_identifiers_parse() {
        // The grammar-level regression: `!keyword` had no word boundary, so
        // ANY identifier starting with a keyword was rejected. Parsed with
        // the raw grammar — schema validation is a separate layer and these
        // names are deliberately not real fields.
        use crate::query::grammar::{QueryParser, Rule};
        use pest::Parser;
        for name in [
            "inherits_from",
            "index",
            "input_count",
            "order_id",
            "notes",
            "byte_size",
            "limit_x",
            "ascending",
            "selected",
            "nullable",
            "contains_x",
            "matches_x",
            "group_by",
            "true_story",
            "increment",
            "order_by",
            "notable",
            "starting_point",
            "grouped",
            "ordering",
            "limited",
        ] {
            for shape in [
                format!("functions where {name} == 1"),
                format!("functions select {name}"),
                format!("functions order by {name} desc"),
                format!("functions group by {name}"),
                format!("functions where {name} contains 'x'"),
            ] {
                assert!(
                    QueryParser::parse(Rule::query, &shape).is_ok(),
                    "`{name}` must parse as a field name: {shape}"
                );
            }
        }
    }

    #[test]
    fn every_schema_field_parses_and_validates() {
        let kinds: &[(EntityType, &str)] = &[
            (EntityType::Modules, "modules"),
            (EntityType::Classes, "classes"),
            (EntityType::Functions, "functions"),
            (EntityType::Methods, "methods"),
            (EntityType::Constants, "constants"),
            (EntityType::Entities, "entities"),
            (EntityType::Imports, "imports"),
            (EntityType::Calls, "calls"),
            (EntityType::Fields, "fields"),
        ];
        for (entity, label) in kinds {
            for spec in fields_for(entity) {
                let q = format!("{label} select {}", spec.name);
                parse_query(&q).unwrap_or_else(|e| panic!("documented field rejected: {q} ({e})"));
            }
        }
    }

    #[test]
    fn keyword_prefixed_documented_fields_are_in_schema() {
        // The names above are the shapes; these are real schema entries whose
        // prefixes are keywords.
        let q = "classes where inherits_from contains 'BaseModel'";
        let parsed = parse_query(q).expect("README example parses");
        validate_query(&parsed).expect("inherits_from is a real class field");
    }

    #[test]
    fn unknown_field_is_rejected_with_the_available_list() {
        let err = parse_query("functions where bogus_field == 1").unwrap_err();
        assert!(err.contains("unknown field 'bogus_field'"), "{err}");
        assert!(err.contains("for functions"), "{err}");
        assert!(err.contains("available:"), "{err}");
        assert!(err.contains("name"), "{err}");

        // Typos in SELECT and ORDER BY fail the same way.
        assert!(parse_query("functions select filepath")
            .unwrap_err()
            .contains("unknown field 'filepath'"));
        assert!(parse_query("classes order by nmae")
            .unwrap_err()
            .contains("unknown field 'nmae'"));
    }

    #[test]
    fn derived_call_arguments_are_exempt() {
        // Derived-call arguments are values, not schema fields (the call
        // implementation decides what they mean), so they bypass validation.
        let parsed = parse_query("functions where has_method(\"__init__\") == true").unwrap();
        validate_query(&parsed).expect("derived call args are implementation-defined");
    }

    #[test]
    fn documented_examples_parse_and_validate() {
        for example in EXAMPLES {
            parse_query(example)
                .unwrap_or_else(|e| panic!("documented example is invalid: {example}\n{e}"));
        }
    }

    /// The docs are generated from this module; a schema edit that forgets
    /// to regenerate them fails here. Regenerate with:
    /// `cargo test -p core_indexer write_query_language_docs -- --ignored`
    #[test]
    fn query_language_docs_are_current() {
        let path = docs_path();
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            on_disk,
            render_markdown(),
            "docs/query-language.md is stale — regenerate it with \
             `cargo test -p core_indexer write_query_language_docs -- --ignored`"
        );
    }

    #[test]
    #[ignore = "regenerates docs/query-language.md"]
    fn write_query_language_docs() {
        std::fs::write(docs_path(), render_markdown()).expect("write docs/query-language.md");
    }

    fn docs_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/query-language.md")
    }
}
