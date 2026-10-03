// CodeRadar v3.6 — Query Execution (§7.1, §7.2a)
// Streaming and aggregated query modes against the in-memory projected graph.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pyo3::prelude::*;

use crate::query::grammar::{
    AggFunc, CompOp, EntityType, Operand, ParsedQuery, Predicate, SelectItem,
};
use crate::query::schema::IDENTITY_FIELDS;
use crate::types::ProjectedGraph;
use crate::types::*;

/// A single result row from a query.
#[derive(Clone, Debug)]
pub struct QueryRow {
    pub fields: HashMap<String, QueryValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueryValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    List(Vec<QueryValue>),
    Null,
}

impl QueryRow {
    pub fn to_pyobject(self, py: Python<'_>) -> PyObject {
        let dict = pyo3::types::PyDict::new(py);
        for (k, v) in &self.fields {
            match v {
                QueryValue::String(s) => {
                    dict.set_item(k, s).ok();
                }
                QueryValue::Int(i) => {
                    dict.set_item(k, i).ok();
                }
                QueryValue::Float(f) => {
                    dict.set_item(k, f).ok();
                }
                QueryValue::Bool(b) => {
                    dict.set_item(k, b).ok();
                }
                QueryValue::List(l) => {
                    let py_list: Vec<PyObject> =
                        l.iter().map(|v| v.clone().to_pyobject(py)).collect();
                    dict.set_item(k, py_list).ok();
                }
                QueryValue::Null => {
                    dict.set_item(k, py.None()).ok();
                }
            }
        }
        dict.into()
    }
}

impl QueryValue {
    pub fn to_pyobject(self, py: Python<'_>) -> PyObject {
        match self {
            // `IntoPy::into_py` is deprecated and goes away in PyO3 0.24;
            // these conversions are all infallible (Error = Infallible), so
            // the unwraps cannot fire.
            QueryValue::String(s) => s.into_pyobject(py).unwrap().into_any().unbind(),
            QueryValue::Int(i) => i.into_pyobject(py).unwrap().into_any().unbind(),
            QueryValue::Float(f) => f.into_pyobject(py).unwrap().into_any().unbind(),
            QueryValue::Bool(b) => b.into_pyobject(py).unwrap().to_owned().into_any().unbind(),
            QueryValue::List(l) => {
                let list: Vec<PyObject> = l.into_iter().map(|v| v.to_pyobject(py)).collect();
                list.into_pyobject(py).unwrap().into_any().unbind()
            }
            QueryValue::Null => py.None(),
        }
    }
}

// ── Lazy field materialisation (plan §2.4) ─────────────────────────────────
//
// Every scan used to build a complete `QueryRow` — including cloned caller and
// callee id lists — for every entity in the graph, evaluate the WHERE clause,
// and drop the row. `functions where name = "x"` allocated the whole graph to
// return one row. Fields are now built twice: once with only what the
// predicate reads, then, for survivors only, with what the output needs.

/// Which fields a caller actually needs.
#[derive(Clone, Debug)]
enum FieldSet {
    All,
    Only(std::collections::HashSet<String>),
}

impl FieldSet {
    fn wants(&self, key: &str) -> bool {
        match self {
            FieldSet::All => true,
            FieldSet::Only(keys) => keys.contains(key),
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, FieldSet::Only(keys) if keys.is_empty())
    }

    /// Fields the WHERE clause reads.
    ///
    /// A derived call looks up fields by names that are its *arguments*
    /// (`contains(decorators, "x")`) or hardcoded in its implementation, so
    /// any of them collapses this to `All`. Guessing narrower would silently
    /// change which rows match, and a query engine that quietly returns the
    /// wrong rows is worse than one that allocates.
    fn for_predicate(pred: Option<&Predicate>) -> Self {
        let mut keys = std::collections::HashSet::new();
        match pred {
            None => FieldSet::Only(keys),
            Some(p) => {
                if collect_predicate_fields(p, &mut keys) {
                    FieldSet::All
                } else {
                    FieldSet::Only(keys)
                }
            }
        }
    }

    /// Fields the result rows need. An empty SELECT means "everything".
    fn for_output(query: &ParsedQuery) -> Self {
        if query.select.is_empty() {
            return FieldSet::All;
        }
        let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        for item in &query.select {
            match item {
                SelectItem::Path(path) => {
                    keys.insert(path.clone());
                }
                // Aggregates are filled in by the group-by phase, but their
                // source column still has to exist to be aggregated over.
                SelectItem::Aggregate { path, .. } => {
                    keys.insert(path.clone());
                }
            }
        }
        if let Some(order) = &query.order_by {
            keys.insert(order.path.clone());
        }
        for path in &query.group_by {
            keys.insert(path.clone());
        }
        FieldSet::Only(keys)
    }
}

/// Collect `Path` operand names; returns true if a derived call was seen, in
/// which case the collected set is not to be trusted.
fn collect_predicate_fields(
    pred: &Predicate,
    keys: &mut std::collections::HashSet<String>,
) -> bool {
    match pred {
        Predicate::Comparison { left, op: _, right } => {
            collect_operand_fields(left, keys) || collect_operand_fields(right, keys)
        }
        Predicate::Not(inner) => collect_predicate_fields(inner, keys),
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            let l = collect_predicate_fields(a, keys);
            let r = collect_predicate_fields(b, keys);
            l || r
        }
    }
}

fn collect_operand_fields(op: &Operand, keys: &mut std::collections::HashSet<String>) -> bool {
    match op {
        Operand::Path(parts) => {
            keys.insert(parts.join("."));
            false
        }
        Operand::ListValue(items) => items
            .iter()
            .map(|i| collect_operand_fields(i, keys))
            .fold(false, |a, b| a || b),
        Operand::DerivedCall { .. } => true,
        Operand::StringValue(_) | Operand::NumberValue(_) | Operand::BoolValue(_) => false,
    }
}

/// Insert a field only when it is wanted — the value expression is not
/// evaluated otherwise, which is the whole point.
macro_rules! put {
    ($fields:expr, $want:expr, $key:literal, $value:expr) => {
        if $want.wants($key) {
            $fields.insert($key.to_string(), $value);
        }
    };
}

/// How many rows a scan may stop after.
///
/// Only when nothing downstream needs to see the rows it would discard:
/// `ORDER BY` picks the top N from all of them, and a grouping counts them.
fn pushdown_limit(query: &ParsedQuery) -> Option<usize> {
    match query.limit {
        Some(limit) if query.order_by.is_none() && query.group_by.is_empty() => {
            Some(limit as usize)
        }
        _ => None,
    }
}

fn reached(rows: &[QueryRow], cap: Option<usize>) -> bool {
    matches!(cap, Some(cap) if rows.len() >= cap)
}

/// The file path an entity's module resolves to (identity field `file_path`).
fn file_path_of(snapshot: &ProjectedGraph, module_id: &str) -> String {
    snapshot
        .modules
        .get(module_id)
        .map(|m| m.path.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

/// The file path encoded in an entity id (``.\src\mod.py::name`` →
/// `src/mod.py`). Used where the entity has no module link (imports).
fn file_from_entity_id(id: &str) -> String {
    let file = id.split("::").next().unwrap_or("");
    file.trim_start_matches('.')
        .trim_start_matches(['\\', '/'])
        .replace('\\', "/")
}

fn function_kind_label(kind: &FunctionKind) -> &'static str {
    match kind {
        FunctionKind::Free => "function",
        FunctionKind::Method => "method",
        FunctionKind::StaticMethod => "static",
        FunctionKind::ClassMethod => "classmethod",
        FunctionKind::Property => "property",
        FunctionKind::PropertySetter => "property_setter",
        FunctionKind::PropertyDeleter => "property_deleter",
        FunctionKind::CachedProperty => "cached_property",
        FunctionKind::AbstractMethod => "abstract",
        FunctionKind::DataclassSynthesized { .. } => "dataclass_synthesized",
    }
}

/// Attach the row-identity fields (plan §3.3) after SELECT projection, so
/// every row can be located and fed back into `callers_of` / `plan_rename`
/// even when the query narrowed the projection.
fn with_identity(
    mut fields: HashMap<String, QueryValue>,
    id: &str,
    file_path: &str,
    kind: &str,
    parent_id: Option<&str>,
) -> HashMap<String, QueryValue> {
    fields.insert("id".to_string(), QueryValue::String(id.to_string()));
    fields.insert(
        "file_path".to_string(),
        QueryValue::String(file_path.to_string()),
    );
    fields.insert("kind".to_string(), QueryValue::String(kind.to_string()));
    fields.insert(
        "parent_id".to_string(),
        match parent_id {
            Some(p) => QueryValue::String(p.to_string()),
            None => QueryValue::Null,
        },
    );
    fields
}

/// Execute a parsed query against a snapshot.
pub fn execute_query(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let rows = match query.entity {
        EntityType::Functions => scan_functions(snapshot, query, false),
        // `methods` is every function defined on a class, whatever its
        // method kind (plan §3.4).
        EntityType::Methods => scan_functions(snapshot, query, true),
        EntityType::Classes => scan_classes(snapshot, query),
        EntityType::Modules => scan_modules(snapshot, query),
        EntityType::Constants => scan_constants(snapshot, query),
        // `entities` unions the name-bearing kinds.
        EntityType::Entities => {
            let mut rows = scan_modules(snapshot, query);
            rows.extend(scan_classes(snapshot, query));
            rows.extend(scan_functions(snapshot, query, false));
            rows.extend(scan_constants(snapshot, query));
            rows
        }
        EntityType::Imports => scan_imports(snapshot, query),
        EntityType::Calls => scan_calls(snapshot, query),
        EntityType::Fields => scan_fields(snapshot, query),
    };

    // Grouping folds rows before ordering: ORDER BY may name an aggregate
    // alias, which only exists once the groups are computed.
    let mut result = apply_group_by(rows, query);
    if let Some(order) = &query.order_by {
        apply_order(&mut result, order);
    }

    // Apply limit
    if let Some(limit) = query.limit {
        result.truncate(limit as usize);
    }

    // Projection runs last so grouping can see the columns it aggregates
    // over. Identity fields (and GROUP BY keys) survive whatever `select`
    // narrows to — they are the row's key (§3.3).
    if !query.select.is_empty() {
        for row in &mut result {
            let keep: Vec<(String, QueryValue)> = IDENTITY_FIELDS
                .iter()
                .map(|s| s.name)
                .chain(query.group_by.iter().map(|p| p.as_str()))
                .filter_map(|k| row.fields.get(k).map(|v| (k.to_string(), v.clone())))
                .collect();
            let mut projected = project_fields(&row.fields, &query.select);
            for (key, value) in keep {
                projected.insert(key, value);
            }
            row.fields = projected;
        }
    }

    result
}

/// Fold rows into groups and compute SELECT aggregates.
///
/// Aggregates used to be stubbed to a literal `0` — `count(*)` looked like a
/// real answer. With no GROUP BY every row forms one group, so
/// `select count(*) as n` returns a single row.
fn apply_group_by(rows: Vec<QueryRow>, query: &ParsedQuery) -> Vec<QueryRow> {
    let aggregates: Vec<&SelectItem> = query
        .select
        .iter()
        .filter(|item| matches!(item, SelectItem::Aggregate { .. }))
        .collect();
    if aggregates.is_empty() && query.group_by.is_empty() {
        return rows;
    }

    // First-seen order keeps ungrouped output stable instead of
    // HashMap-random.
    let mut groups: Vec<(Vec<String>, Vec<QueryRow>)> = Vec::new();
    let mut index: std::collections::HashMap<Vec<String>, usize> = std::collections::HashMap::new();
    for row in rows {
        let key: Vec<String> = query
            .group_by
            .iter()
            .map(|path| value_to_string(row.fields.get(path).unwrap_or(&QueryValue::Null)))
            .collect();
        match index.get(&key) {
            Some(&i) => groups[i].1.push(row),
            None => {
                index.insert(key.clone(), groups.len());
                groups.push((key, vec![row]));
            }
        }
    }

    groups
        .into_iter()
        .map(|(_, group)| aggregate_group(&group, query, &aggregates))
        .collect()
}

fn aggregate_group(
    group: &[QueryRow],
    query: &ParsedQuery,
    aggregates: &[&SelectItem],
) -> QueryRow {
    let first = &group[0];
    let mut fields: HashMap<String, QueryValue> = HashMap::new();

    // Group keys and any non-aggregate SELECT paths come from a
    // representative row; SQL would reject the latter when it is neither
    // grouped nor aggregated, but this is a graph query, not a database.
    for path in &query.group_by {
        if let Some(value) = first.fields.get(path) {
            fields.insert(path.clone(), value.clone());
        }
    }
    if query.select.is_empty() {
        for (key, value) in &first.fields {
            fields.entry(key.clone()).or_insert_with(|| value.clone());
        }
    } else {
        for item in &query.select {
            if let SelectItem::Path(path) = item {
                if let Some(value) = first.fields.get(path) {
                    fields.insert(path.clone(), value.clone());
                }
            }
        }
    }

    for item in aggregates {
        let SelectItem::Aggregate { func, path, alias } = item else {
            continue;
        };
        let value = if path == "*" {
            QueryValue::Int(group.len() as i64)
        } else {
            let present: Vec<&QueryValue> = group
                .iter()
                .filter_map(|row| row.fields.get(path))
                .filter(|value| !matches!(value, QueryValue::Null))
                .collect();
            let numbers: Vec<f64> = present
                .iter()
                .filter_map(|value| match value {
                    QueryValue::Int(n) => Some(*n as f64),
                    QueryValue::Float(f) => Some(*f),
                    _ => None,
                })
                .collect();
            match func {
                AggFunc::Count => QueryValue::Int(present.len() as i64),
                AggFunc::Sum => match numbers.iter().sum::<f64>() {
                    total if numbers.is_empty() => {
                        let _ = total;
                        QueryValue::Null
                    }
                    total => QueryValue::Float(total),
                },
                AggFunc::Avg => {
                    if numbers.is_empty() {
                        QueryValue::Null
                    } else {
                        QueryValue::Float(numbers.iter().sum::<f64>() / numbers.len() as f64)
                    }
                }
                AggFunc::Min => match numbers.iter().copied().reduce(f64::min) {
                    Some(min) => QueryValue::Float(min),
                    None => present
                        .first()
                        .map(|v| (*v).clone())
                        .unwrap_or(QueryValue::Null),
                },
                AggFunc::Max => match numbers.iter().copied().reduce(f64::max) {
                    Some(max) => QueryValue::Float(max),
                    None => present
                        .last()
                        .map(|v| (*v).clone())
                        .unwrap_or(QueryValue::Null),
                },
            }
        };
        fields.insert(alias.clone(), value);
    }

    // A group is not an entity: it has no id to hand to `callers_of`. The
    // `kind` slot is shared with the identity kind, so a `group by kind`
    // keeps the grouped value — that is the answer the user asked for.
    fields.insert("id".to_string(), QueryValue::Null);
    fields.insert("file_path".to_string(), QueryValue::Null);
    fields
        .entry("kind".to_string())
        .or_insert_with(|| QueryValue::String("group".to_string()));
    fields.insert("parent_id".to_string(), QueryValue::Null);
    QueryRow { fields }
}

fn scan_functions(
    snapshot: &ProjectedGraph,
    query: &ParsedQuery,
    methods_only: bool,
) -> Vec<QueryRow> {
    let probe_want = FieldSet::for_predicate(query.where_clause.as_ref());
    let output_want = FieldSet::for_output(query);
    let cap = pushdown_limit(query);
    let mut rows = Vec::new();

    for (_id, fn_val) in snapshot.functions.iter() {
        if methods_only && fn_val.parent_class.is_none() {
            continue;
        }
        if let Some(pred) = &query.where_clause {
            let probe = function_fields(fn_val, snapshot, &probe_want);
            if !evaluate_predicate(pred, &probe) {
                continue;
            }
        }
        let fields = function_fields(fn_val, snapshot, &output_want);
        let fields = with_identity(
            fields,
            &fn_val.id,
            &file_path_of(snapshot, &fn_val.parent_module),
            function_kind_label(&fn_val.kind),
            fn_val.parent_class.as_deref(),
        );
        rows.push(QueryRow { fields });
        if reached(&rows, cap) {
            break;
        }
    }
    rows
}

fn function_fields(
    fn_val: &Function,
    snapshot: &ProjectedGraph,
    want: &FieldSet,
) -> HashMap<String, QueryValue> {
    let mut fields = HashMap::new();
    if want.is_empty() {
        return fields;
    }

    put!(
        fields,
        want,
        "name",
        QueryValue::String(fn_val.name.clone())
    );
    put!(fields, want, "line", QueryValue::Int(fn_val.line as i64));
    put!(
        fields,
        want,
        "line_count",
        QueryValue::Int((fn_val.exit_line - fn_val.line) as i64)
    );
    put!(fields, want, "is_async", QueryValue::Bool(fn_val.is_async));
    put!(
        fields,
        want,
        "decorators",
        QueryValue::List(
            fn_val
                .decorators
                .iter()
                .map(|d| QueryValue::String(d.clone()))
                .collect()
        )
    );

    // ── Reverse-index enrichment ──────────────────────────────────
    put!(
        fields,
        want,
        "caller_count",
        QueryValue::Int(
            snapshot
                .callers_by_callee
                .get(fn_val.id.as_str())
                .map(|s| s.len() as i64)
                .unwrap_or(0)
        )
    );
    put!(
        fields,
        want,
        "callee_count",
        QueryValue::Int(
            snapshot
                .callees_by_caller
                .get(fn_val.id.as_str())
                .map(|s| s.len() as i64)
                .unwrap_or(0)
        )
    );
    put!(
        fields,
        want,
        "callers",
        QueryValue::List(
            snapshot
                .callers_by_callee
                .get(fn_val.id.as_str())
                .map(|s| s.iter().map(|id| QueryValue::String(id.clone())).collect())
                .unwrap_or_default()
        )
    );
    put!(
        fields,
        want,
        "callees",
        QueryValue::List(
            snapshot
                .callees_by_caller
                .get(fn_val.id.as_str())
                .map(|s| s.iter().map(|id| QueryValue::String(id.clone())).collect())
                .unwrap_or_default()
        )
    );

    // Resolved calls as entity IDs
    put!(
        fields,
        want,
        "resolved_call_targets",
        QueryValue::List(
            fn_val
                .resolved_calls
                .iter()
                .filter_map(|rc| match rc {
                    ResolvedCall::Function(id)
                    | ResolvedCall::Method { method: id, .. }
                    | ResolvedCall::Constructor(id) => Some(QueryValue::String(id.clone())),
                    ResolvedCall::External(s) => Some(QueryValue::String(s.clone())),
                    ResolvedCall::Builtin(s) => Some(QueryValue::String(s.clone())),
                    _ => None,
                })
                .collect()
        )
    );
    put!(
        fields,
        want,
        "parameter_count",
        QueryValue::Int(fn_val.parameters.len() as i64)
    );
    // Plan §3.4: the fields downstream tools kept re-deriving.
    put!(
        fields,
        want,
        "kind",
        QueryValue::String(function_kind_label(&fn_val.kind).to_string())
    );
    put!(
        fields,
        want,
        "parent_class",
        QueryValue::String(fn_val.parent_class.clone().unwrap_or_default())
    );
    put!(
        fields,
        want,
        "is_override",
        QueryValue::Bool(snapshot.overrides_base.contains_key(fn_val.id.as_str()))
    );
    put!(
        fields,
        want,
        "complexity",
        QueryValue::Int(fn_val.metrics.cyclomatic as i64)
    );
    put!(
        fields,
        want,
        "docstring",
        QueryValue::String(fn_val.docstring.clone().unwrap_or_default())
    );
    if let Some(ref rt) = fn_val.return_type {
        put!(fields, want, "return_type", QueryValue::String(rt.clone()));
    }

    fields
}

fn scan_classes(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let probe_want = FieldSet::for_predicate(query.where_clause.as_ref());
    let output_want = FieldSet::for_output(query);
    let cap = pushdown_limit(query);
    let mut rows = Vec::new();

    for (_id, cls) in snapshot.classes.iter() {
        if let Some(pred) = &query.where_clause {
            if !evaluate_predicate(pred, &class_fields(cls, snapshot, &probe_want)) {
                continue;
            }
        }
        let fields = class_fields(cls, snapshot, &output_want);
        let fields = with_identity(
            fields,
            &cls.id,
            &file_path_of(snapshot, &cls.parent_module),
            "class",
            cls.parent_class.as_deref(),
        );
        rows.push(QueryRow { fields });
        if reached(&rows, cap) {
            break;
        }
    }
    rows
}

fn class_fields(
    cls: &Class,
    snapshot: &ProjectedGraph,
    want: &FieldSet,
) -> HashMap<String, QueryValue> {
    let mut fields = HashMap::new();
    if want.is_empty() {
        return fields;
    }
    put!(fields, want, "name", QueryValue::String(cls.name.clone()));
    put!(fields, want, "line", QueryValue::Int(cls.line as i64));
    put!(
        fields,
        want,
        "method_count",
        QueryValue::Int(cls.methods.len() as i64)
    );
    put!(
        fields,
        want,
        "decorators",
        QueryValue::List(
            cls.decorators
                .iter()
                .map(|d| QueryValue::String(d.clone()))
                .collect()
        )
    );
    // Plan §3.4: inheritance surface and class facts.
    put!(
        fields,
        want,
        "bases",
        QueryValue::List(
            cls.bases
                .iter()
                .map(|b| {
                    QueryValue::String(if b.path.is_empty() {
                        b.name.clone()
                    } else {
                        format!("{}.{}", b.path.join("."), b.name)
                    })
                })
                .collect()
        )
    );
    put!(
        fields,
        want,
        "base_ids",
        QueryValue::List(
            cls.resolved_bases
                .iter()
                .map(|id| QueryValue::String(id.clone()))
                .collect()
        )
    );
    // Transitive ancestors from the MRO — in-repo ids plus external base
    // names, so `inherits_from contains 'BaseModel'` works for libraries.
    // The MRO starts with the class itself (Python semantics); ancestors
    // must not, or every class would "inherit from" itself.
    put!(
        fields,
        want,
        "inherits_from",
        QueryValue::List(
            cls.mro
                .iter()
                .filter(|node| !matches!(node, MroNode::Class(id) if id == &cls.id))
                .map(|node| match node {
                    MroNode::Class(id) => QueryValue::String(id.clone()),
                    MroNode::External { name } => QueryValue::String(name.clone()),
                })
                .collect()
        )
    );
    let is_abstract = matches!(
        cls.effective,
        EffectiveClass::Abstract | EffectiveClass::Protocol
    ) || cls.methods.iter().any(|mid| {
        snapshot
            .functions
            .get(mid)
            .is_some_and(|f| matches!(f.kind, FunctionKind::AbstractMethod))
    });
    put!(fields, want, "is_abstract", QueryValue::Bool(is_abstract));
    put!(
        fields,
        want,
        "docstring",
        QueryValue::String(cls.docstring.clone().unwrap_or_default())
    );
    fields
}

fn scan_modules(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let probe_want = FieldSet::for_predicate(query.where_clause.as_ref());
    let output_want = FieldSet::for_output(query);
    let cap = pushdown_limit(query);
    let mut rows = Vec::new();

    for (_id, module) in snapshot.modules.iter() {
        if let Some(pred) = &query.where_clause {
            if !evaluate_predicate(pred, &module_fields(module, &probe_want)) {
                continue;
            }
        }
        let fields = module_fields(module, &output_want);
        let fields = with_identity(
            fields,
            &module.id,
            &module.path.to_string_lossy().replace('\\', "/"),
            "module",
            None,
        );
        rows.push(QueryRow { fields });
        if reached(&rows, cap) {
            break;
        }
    }
    rows
}

fn scan_constants(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let probe_want = FieldSet::for_predicate(query.where_clause.as_ref());
    let output_want = FieldSet::for_output(query);
    let cap = pushdown_limit(query);
    let mut rows = Vec::new();

    for (_id, constant) in snapshot.constants.iter() {
        if let Some(pred) = &query.where_clause {
            if !evaluate_predicate(pred, &constant_fields(constant, &probe_want)) {
                continue;
            }
        }
        let fields = constant_fields(constant, &output_want);
        let fields = with_identity(
            fields,
            &constant.id,
            &file_from_entity_id(&constant.id),
            "constant",
            None,
        );
        rows.push(QueryRow { fields });
        if reached(&rows, cap) {
            break;
        }
    }
    rows
}

fn constant_fields(constant: &Constant, want: &FieldSet) -> HashMap<String, QueryValue> {
    let mut fields = HashMap::new();
    if want.is_empty() {
        return fields;
    }
    put!(
        fields,
        want,
        "name",
        QueryValue::String(constant.name.clone())
    );
    put!(
        fields,
        want,
        "value",
        QueryValue::String(constant.default_value.clone().unwrap_or_default())
    );
    put!(
        fields,
        want,
        "annotation",
        QueryValue::String(constant.annotation.clone().unwrap_or_default())
    );
    fields
}

fn module_fields(module: &Module, want: &FieldSet) -> HashMap<String, QueryValue> {
    let mut fields = HashMap::new();
    if want.is_empty() {
        return fields;
    }
    put!(
        fields,
        want,
        "name",
        QueryValue::String(module.name.clone())
    );
    put!(
        fields,
        want,
        "path",
        QueryValue::String(module.path.to_string_lossy().to_string())
    );
    put!(
        fields,
        want,
        "language",
        QueryValue::String(format!("{:?}", module.language))
    );
    put!(
        fields,
        want,
        "class_count",
        QueryValue::Int(module.classes.len() as i64)
    );
    put!(
        fields,
        want,
        "function_count",
        QueryValue::Int(module.functions.len() as i64)
    );
    put!(
        fields,
        want,
        "import_count",
        QueryValue::Int(module.imports.len() as i64)
    );
    fields
}

fn scan_imports(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let probe_want = FieldSet::for_predicate(query.where_clause.as_ref());
    let output_want = FieldSet::for_output(query);
    let cap = pushdown_limit(query);
    let mut rows = Vec::new();

    for (_id, import) in snapshot.imports.iter() {
        if let Some(pred) = &query.where_clause {
            if !evaluate_predicate(pred, &import_fields(import, &probe_want)) {
                continue;
            }
        }
        let fields = import_fields(import, &output_want);
        let fields = with_identity(
            fields,
            &import.id,
            &file_from_entity_id(&import.id),
            "import",
            None,
        );
        rows.push(QueryRow { fields });
        if reached(&rows, cap) {
            break;
        }
    }
    rows
}

fn import_fields(import: &Import, want: &FieldSet) -> HashMap<String, QueryValue> {
    let mut fields = HashMap::new();
    if want.is_empty() {
        return fields;
    }
    put!(fields, want, "raw", QueryValue::String(import.raw.clone()));
    // `kind` is the row-identity kind ("import"); the import's own flavour
    // lives in `import_kind` so `imports where kind == 'import'` stays true.
    put!(
        fields,
        want,
        "import_kind",
        QueryValue::String(
            match &import.kind {
                ImportKind::ModuleImport { .. } => "module",
                ImportKind::FromImport { .. } => "from",
                ImportKind::RelativeImport { .. } => "relative",
                ImportKind::StarImport { .. } => "star",
                ImportKind::Side { .. } => "side",
            }
            .to_string()
        )
    );
    put!(fields, want, "line", QueryValue::Int(import.line as i64));
    put!(
        fields,
        want,
        "is_type_only",
        QueryValue::Bool(import.is_type_only)
    );

    // Resolved target + its kind, derived from the resolution variant so
    // `imports where target_kind == "function"` / `... == "external"` work.
    match &import.resolution {
        ImportResolution::Module(id) | ImportResolution::Symbol(SymbolId::Module(id)) => {
            put!(
                fields,
                want,
                "resolved_module",
                QueryValue::String(id.clone())
            );
            put!(
                fields,
                want,
                "target_kind",
                QueryValue::String("module".into())
            );
        }
        ImportResolution::Symbol(SymbolId::Function(id)) => {
            put!(
                fields,
                want,
                "resolved_target",
                QueryValue::String(id.clone())
            );
            put!(
                fields,
                want,
                "target_kind",
                QueryValue::String("function".into())
            );
        }
        ImportResolution::Symbol(SymbolId::Class(id)) => {
            put!(
                fields,
                want,
                "resolved_target",
                QueryValue::String(id.clone())
            );
            put!(
                fields,
                want,
                "target_kind",
                QueryValue::String("class".into())
            );
        }
        ImportResolution::Symbol(SymbolId::Import(id)) => {
            put!(
                fields,
                want,
                "resolved_target",
                QueryValue::String(id.clone())
            );
            put!(
                fields,
                want,
                "target_kind",
                QueryValue::String("import".into())
            );
        }
        ImportResolution::External { distribution } => {
            put!(
                fields,
                want,
                "resolved_target",
                QueryValue::String(distribution.clone().unwrap_or_else(|| "external".into()))
            );
            put!(
                fields,
                want,
                "target_kind",
                QueryValue::String("external".into())
            );
        }
        ImportResolution::Wildcard { module, .. } => {
            put!(
                fields,
                want,
                "resolved_module",
                QueryValue::String(module.clone())
            );
            put!(
                fields,
                want,
                "target_kind",
                QueryValue::String("wildcard".into())
            );
        }
        ImportResolution::Dynamic => {
            put!(
                fields,
                want,
                "target_kind",
                QueryValue::String("dynamic".into())
            );
        }
        ImportResolution::Unresolved => {
            put!(
                fields,
                want,
                "target_kind",
                QueryValue::String("unresolved".into())
            );
        }
    }
    fields
}

fn scan_calls(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let probe_want = FieldSet::for_predicate(query.where_clause.as_ref());
    let output_want = FieldSet::for_output(query);
    let cap = pushdown_limit(query);
    let mut rows = Vec::new();

    // Produce one row per call edge from the callers_by_callee reverse index
    for (source_id, callees) in snapshot.callees_by_caller.iter() {
        for target_id in callees.iter() {
            if let Some(pred) = &query.where_clause {
                let probe = call_fields(source_id, target_id, snapshot, &probe_want);
                if !evaluate_predicate(pred, &probe) {
                    continue;
                }
            }
            let fields = call_fields(source_id, target_id, snapshot, &output_want);
            // A call row has no single entity id of its own: its key is the
            // (caller, callee) pair.
            let fields = with_identity(
                fields,
                &format!("{source_id}->{target_id}"),
                &snapshot
                    .functions
                    .get(source_id)
                    .map(|f| file_path_of(snapshot, &f.parent_module))
                    .unwrap_or_default(),
                "call",
                Some(source_id),
            );
            rows.push(QueryRow { fields });
            if reached(&rows, cap) {
                return rows;
            }
        }
    }
    rows
}

fn call_fields(
    source_id: &str,
    target_id: &str,
    snapshot: &ProjectedGraph,
    want: &FieldSet,
) -> HashMap<String, QueryValue> {
    let mut fields = HashMap::new();
    if want.is_empty() {
        return fields;
    }
    put!(
        fields,
        want,
        "source",
        QueryValue::String(source_id.to_string())
    );
    put!(
        fields,
        want,
        "target",
        QueryValue::String(target_id.to_string())
    );
    put!(
        fields,
        want,
        "target_kind",
        QueryValue::String(if snapshot.functions.contains_key(target_id) {
            "function".to_string()
        } else if snapshot.classes.contains_key(target_id) {
            "class".to_string()
        } else {
            "external".to_string()
        })
    );
    fields
}

fn scan_fields(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let probe_want = FieldSet::for_predicate(query.where_clause.as_ref());
    let output_want = FieldSet::for_output(query);
    let cap = pushdown_limit(query);
    let mut rows = Vec::new();

    // Fields live on classes — iterate classes and emit each field as a row
    for (_class_id, cls) in snapshot.classes.iter() {
        for field in cls.fields.iter() {
            if let Some(pred) = &query.where_clause {
                if !evaluate_predicate(pred, &field_fields(field, cls, &probe_want)) {
                    continue;
                }
            }
            let fields = field_fields(field, cls, &output_want);
            let fields = with_identity(
                fields,
                &format!("{}::{}", cls.id, field.name),
                &file_path_of(snapshot, &cls.parent_module),
                "field",
                Some(&cls.id),
            );
            rows.push(QueryRow { fields });
            if reached(&rows, cap) {
                return rows;
            }
        }
    }
    rows
}

fn field_fields(field: &Field, cls: &Class, want: &FieldSet) -> HashMap<String, QueryValue> {
    let mut fields = HashMap::new();
    if want.is_empty() {
        return fields;
    }
    put!(fields, want, "name", QueryValue::String(field.name.clone()));
    put!(
        fields,
        want,
        "parent_class",
        QueryValue::String(cls.id.clone())
    );
    if let Some(ref ann) = field.annotation {
        put!(
            fields,
            want,
            "type_annotation",
            QueryValue::String(ann.clone())
        );
    }
    put!(
        fields,
        want,
        "is_class_var",
        QueryValue::Bool(field.is_class_var)
    );
    fields
}

/// Evaluate a WHERE predicate against a row's field values.
fn evaluate_predicate(pred: &Predicate, fields: &HashMap<String, QueryValue>) -> bool {
    match pred {
        Predicate::Comparison { left, op, right } => {
            let l_val = resolve_operand(left, fields);
            let r_val = resolve_operand(right, fields);
            compare_values(op, &l_val, &r_val)
        }
        Predicate::Not(inner) => !evaluate_predicate(inner, fields),
        Predicate::And(a, b) => evaluate_predicate(a, fields) && evaluate_predicate(b, fields),
        Predicate::Or(a, b) => evaluate_predicate(a, fields) || evaluate_predicate(b, fields),
    }
}

fn resolve_operand(op: &Operand, fields: &HashMap<String, QueryValue>) -> QueryValue {
    match op {
        Operand::Path(parts) => {
            let name = parts.join(".");
            fields.get(&name).cloned().unwrap_or(QueryValue::Null)
        }
        Operand::StringValue(s) => QueryValue::String(s.clone()),
        Operand::NumberValue(n) => QueryValue::Float(*n),
        Operand::BoolValue(b) => QueryValue::Bool(*b),
        Operand::ListValue(items) => {
            QueryValue::List(items.iter().map(|i| resolve_operand(i, fields)).collect())
        }
        Operand::DerivedCall { name, args } => evaluate_derived_call(name, args, fields),
    }
}

fn evaluate_derived_call(
    name: &str,
    args: &[Operand],
    fields: &HashMap<String, QueryValue>,
) -> QueryValue {
    match name {
        "inherits_from" => {
            // Check if parent_class field contains the given class name
            let target = args
                .first()
                .map(|a| operand_to_string(a))
                .unwrap_or_default();
            let parent = fields
                .get("parent_class")
                .map(|v| value_to_string(v))
                .unwrap_or_default();
            QueryValue::Bool(parent.contains(&target))
        }
        "contains" => {
            // Check if a list field contains the given value
            if args.len() < 2 {
                return QueryValue::Bool(false);
            }
            let list_field = operand_to_string(&args[0]);
            let search = operand_to_string(&args[1]);
            if let Some(QueryValue::List(items)) = fields.get(&list_field) {
                let found = items
                    .iter()
                    .any(|item| value_to_string(item).contains(&search));
                QueryValue::Bool(found)
            } else if let Some(val) = fields.get(&list_field) {
                QueryValue::Bool(value_to_string(val).contains(&search))
            } else {
                QueryValue::Bool(false)
            }
        }
        "has_method" => {
            let target = args
                .first()
                .map(|a| operand_to_string(a))
                .unwrap_or_default();
            let methods = fields.get("method_names").or_else(|| fields.get("methods"));
            match methods {
                Some(QueryValue::List(items)) => {
                    QueryValue::Bool(items.iter().any(|item| value_to_string(item) == target))
                }
                _ => QueryValue::Bool(false),
            }
        }
        "overrides_of" => {
            // Present when parent_class is set AND the method exists on both
            let parent = fields
                .get("parent_class")
                .map(|v| value_to_string(v))
                .unwrap_or_default();
            QueryValue::Bool(!parent.is_empty())
        }
        _ => QueryValue::Bool(false),
    }
}

fn operand_to_string(op: &Operand) -> String {
    match op {
        Operand::StringValue(s) => s.clone(),
        Operand::Path(p) => p.join("."),
        Operand::NumberValue(n) => format!("{}", n),
        Operand::BoolValue(b) => format!("{}", b),
        _ => String::new(),
    }
}

fn value_to_string(val: &QueryValue) -> String {
    match val {
        QueryValue::String(s) => s.clone(),
        QueryValue::Int(i) => format!("{}", i),
        QueryValue::Float(f) => format!("{}", f),
        QueryValue::Bool(b) => format!("{}", b),
        QueryValue::Null => "null".into(),
        QueryValue::List(_) => String::new(),
    }
}

fn compare_numbers(op: &CompOp, l: f64, r: f64) -> bool {
    match op {
        CompOp::Eq => (l - r).abs() < f64::EPSILON,
        CompOp::NotEq => (l - r).abs() >= f64::EPSILON,
        CompOp::Less => l < r,
        CompOp::Greater => l > r,
        CompOp::LessEq => l <= r,
        CompOp::GreaterEq => l >= r,
        _ => false,
    }
}

fn compare_values(op: &CompOp, left: &QueryValue, right: &QueryValue) -> bool {
    match (left, right) {
        (QueryValue::String(l), QueryValue::String(r)) => match op {
            CompOp::Eq => l == r,
            CompOp::NotEq => l != r,
            CompOp::Contains => l.contains(r),
            CompOp::Matches => {
                // Simple regex matching
                l.contains(r)
            }
            CompOp::StartsWith => l.starts_with(r.as_str()),
            CompOp::EndsWith => l.ends_with(r.as_str()),
            _ => false,
        },
        // `inherits_from contains 'BaseModel'`: list membership by substring,
        // matching the string `contains` semantics.
        (QueryValue::List(items), QueryValue::String(r)) => match op {
            CompOp::Contains => items
                .iter()
                .any(|item| value_to_string(item).contains(r.as_str())),
            CompOp::Eq => items.iter().any(|item| value_to_string(item) == *r),
            CompOp::NotEq => !items.iter().any(|item| value_to_string(item) == *r),
            _ => false,
        },
        // `'x' in decorators`.
        (QueryValue::String(l), QueryValue::List(items)) => match op {
            CompOp::In => items.iter().any(|item| value_to_string(item) == *l),
            _ => false,
        },
        (QueryValue::Int(l), QueryValue::Int(r)) => match op {
            CompOp::Eq => l == r,
            CompOp::NotEq => l != r,
            CompOp::Less => l < r,
            CompOp::Greater => l > r,
            CompOp::LessEq => l <= r,
            CompOp::GreaterEq => l >= r,
            _ => false,
        },
        (QueryValue::Float(l), QueryValue::Float(r)) => match op {
            CompOp::Eq => (l - r).abs() < f64::EPSILON,
            CompOp::NotEq => (l - r).abs() >= f64::EPSILON,
            CompOp::Less => l < r,
            CompOp::Greater => l > r,
            CompOp::LessEq => l <= r,
            CompOp::GreaterEq => l >= r,
            _ => false,
        },
        (QueryValue::Int(l), QueryValue::Float(r)) => compare_numbers(op, *l as f64, *r),
        (QueryValue::Float(l), QueryValue::Int(r)) => compare_numbers(op, *l, *r as f64),
        (QueryValue::Bool(l), QueryValue::Bool(r)) => match op {
            CompOp::Eq => l == r,
            CompOp::NotEq => l != r,
            _ => false,
        },
        _ => false,
    }
}

fn project_fields(
    fields: &HashMap<String, QueryValue>,
    select: &[SelectItem],
) -> HashMap<String, QueryValue> {
    let mut result = HashMap::new();
    for item in select {
        match item {
            SelectItem::Path(path) => {
                if let Some(val) = fields.get(path) {
                    result.insert(path.clone(), val.clone());
                }
            }
            SelectItem::Aggregate {
                func: _,
                path: _,
                alias,
            } => {
                // Already computed by `apply_group_by` — projection must not
                // overwrite it (it used to insert a literal 0 here).
                if let Some(val) = fields.get(alias) {
                    result.insert(alias.clone(), val.clone());
                }
            }
        }
    }
    result
}

fn apply_order(rows: &mut [QueryRow], order: &crate::query::grammar::OrderBy) {
    let path = order.path.clone();
    let desc = matches!(order.direction, crate::query::grammar::OrderDir::Desc);
    rows.sort_by(|a, b| {
        let a_val = a.fields.get(&path);
        let b_val = b.fields.get(&path);
        let cmp = match (a_val, b_val) {
            (Some(QueryValue::String(a)), Some(QueryValue::String(b))) => a.cmp(b),
            (Some(QueryValue::Int(a)), Some(QueryValue::Int(b))) => a.cmp(b),
            (Some(QueryValue::Float(a)), Some(QueryValue::Float(b))) => {
                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
            }
            _ => std::cmp::Ordering::Equal,
        };
        if desc {
            cmp.reverse()
        } else {
            cmp
        }
    });
}

// ── Python Query Iterator (§7.2a) ──────────────────────────────────────────

#[pyclass]
pub struct QueryIterator {
    rows: Vec<QueryRow>,
    position: usize,
    cancelled: Arc<AtomicBool>,
    check_interval: usize,
    items_since_check: usize,
}

#[pymethods]
impl QueryIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<Option<PyObject>> {
        self.items_since_check += 1;
        if self.items_since_check >= self.check_interval {
            self.items_since_check = 0;
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(pyo3::exceptions::PyKeyboardInterrupt::new_err(
                    "query cancelled",
                ));
            }
            py.check_signals()?;
        }
        if self.position >= self.rows.len() {
            return Ok(None);
        }
        let row = self.rows[self.position].clone();
        self.position += 1;
        Ok(Some(row.to_pyobject(py)))
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl QueryIterator {
    pub fn new(rows: Vec<QueryRow>) -> Self {
        Self {
            rows,
            position: 0,
            cancelled: Arc::new(AtomicBool::new(false)),
            check_interval: 64,
            items_since_check: 0,
        }
    }
}

#[cfg(test)]
mod group_by_tests {
    use super::*;
    use crate::query::grammar::parse_query;

    fn row(name: &str, kind: &str, line: i64) -> QueryRow {
        let mut fields = HashMap::new();
        fields.insert("name".to_string(), QueryValue::String(name.to_string()));
        fields.insert("kind".to_string(), QueryValue::String(kind.to_string()));
        fields.insert("line".to_string(), QueryValue::Int(line));
        QueryRow { fields }
    }

    #[test]
    fn count_star_counts_rows_instead_of_returning_zero() {
        let query = parse_query("functions select count(*) as n").unwrap();
        let rows = apply_group_by(vec![row("a", "function", 1), row("b", "method", 2)], &query);
        assert_eq!(rows.len(), 1, "no GROUP BY means one group");
        assert_eq!(rows[0].fields["n"], QueryValue::Int(2));
        // A group is not an entity — no id to feed back into the API.
        assert_eq!(rows[0].fields["id"], QueryValue::Null);
        assert_eq!(rows[0].fields["kind"], QueryValue::String("group".into()));
    }

    #[test]
    fn group_by_splits_rows_and_aggregates_per_group() {
        let query = parse_query("functions select kind, avg(line) as mean group by kind").unwrap();
        let rows = apply_group_by(
            vec![
                row("a", "function", 10),
                row("b", "function", 20),
                row("c", "method", 5),
            ],
            &query,
        );
        assert_eq!(rows.len(), 2, "one row per distinct kind");
        let mut by_kind: HashMap<String, QueryValue> = HashMap::new();
        for r in &rows {
            // The identity `kind` slot carries the grouped value here, not
            // the literal "group".
            let kind = value_to_string(&r.fields["kind"]);
            by_kind.insert(kind, r.fields["mean"].clone());
        }
        assert_eq!(by_kind["function"], QueryValue::Float(15.0));
        assert_eq!(by_kind["method"], QueryValue::Float(5.0));
    }

    #[test]
    fn grouping_by_another_field_still_marks_the_row_as_a_group() {
        let query = parse_query("functions select avg(line) as mean group by name").unwrap();
        let rows = apply_group_by(vec![row("a", "function", 10)], &query);
        assert_eq!(rows[0].fields["kind"], QueryValue::String("group".into()));
    }

    #[test]
    fn count_of_a_column_skips_missing_values() {
        let query = parse_query("functions select count(line) as n").unwrap();
        let mut no_line = row("a", "function", 1);
        no_line.fields.remove("line");
        let rows = apply_group_by(vec![no_line, row("b", "function", 2)], &query);
        assert_eq!(rows[0].fields["n"], QueryValue::Int(1));
    }

    #[test]
    fn projection_does_not_clobber_computed_aggregates() {
        // `project_fields` used to insert a literal 0 for every aggregate,
        // overwriting what the grouping had just computed.
        let query = parse_query("functions select count(*) as n group by kind").unwrap();
        let rows = apply_group_by(
            vec![row("a", "function", 1), row("b", "function", 2)],
            &query,
        );
        let projected = project_fields(&rows[0].fields, &query.select);
        assert_eq!(projected["n"], QueryValue::Int(2));
    }
}
