// CodeRadar v3.6 — Query Execution (§7.1, §7.2a)
// Streaming and aggregated query modes against the in-memory projected graph.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pyo3::prelude::*;

use crate::types::ProjectedGraph;
use crate::types::*;
use crate::query::grammar::{CompOp, EntityType, Operand, ParsedQuery, Predicate, SelectItem};

/// A single result row from a query.
#[derive(Clone, Debug)]
pub struct QueryRow {
    pub fields: HashMap<String, QueryValue>,
}

#[derive(Clone, Debug)]
pub enum QueryValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    List(Vec<QueryValue>),
    Null,
}

impl QueryRow {
    pub fn into_pyobject(self, py: Python<'_>) -> PyObject {
        let dict = pyo3::types::PyDict::new(py);
        for (k, v) in &self.fields {
            match v {
                QueryValue::String(s) => { dict.set_item(k, s).ok(); }
                QueryValue::Int(i) => { dict.set_item(k, i).ok(); }
                QueryValue::Float(f) => { dict.set_item(k, f).ok(); }
                QueryValue::Bool(b) => { dict.set_item(k, b).ok(); }
                QueryValue::List(l) => {
                    let py_list: Vec<PyObject> = l.iter().map(|v| v.clone().into_pyobject(py)).collect();
                    dict.set_item(k, py_list).ok();
                }
                QueryValue::Null => { dict.set_item(k, py.None()).ok(); }
            }
        }
        dict.into()
    }
}

impl QueryValue {
    pub fn into_pyobject(self, py: Python<'_>) -> PyObject {
        match self {
            QueryValue::String(s) => s.into_py(py),
            QueryValue::Int(i) => i.into_py(py),
            QueryValue::Float(f) => f.into_py(py),
            QueryValue::Bool(b) => b.into_py(py),
            QueryValue::List(l) => {
                let list: Vec<PyObject> = l.into_iter().map(|v| v.into_pyobject(py)).collect();
                list.into_py(py)
            }
            QueryValue::Null => py.None(),
        }
    }
}

/// Execute a parsed query against a snapshot.
pub fn execute_query(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let rows = match query.entity {
        EntityType::Functions => scan_functions(snapshot, query),
        EntityType::Classes => scan_classes(snapshot, query),
        EntityType::Modules => scan_modules(snapshot, query),
        EntityType::Imports => scan_imports(snapshot, query),
        EntityType::Calls => scan_calls(snapshot, query),
        EntityType::Fields => scan_fields(snapshot, query),
    };

    // Apply ordering if specified
    let mut result = rows;
    if let Some(order) = &query.order_by {
        apply_order(&mut result, order);
    }

    // Apply limit
    if let Some(limit) = query.limit {
        result.truncate(limit as usize);
    }

    result
}

fn scan_functions(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let mut rows = Vec::new();
    for (_id, fn_val) in snapshot.functions.iter() {

        // Build a row with all function fields
        let mut fields = HashMap::new();
        fields.insert("name".to_string(), QueryValue::String(fn_val.name.clone()));
        fields.insert(
            "line".to_string(),
            QueryValue::Int(fn_val.line as i64),
        );
        fields.insert(
            "line_count".to_string(),
            QueryValue::Int((fn_val.exit_line - fn_val.line) as i64),
        );
        fields.insert(
            "is_async".to_string(),
            QueryValue::Bool(fn_val.is_async),
        );
        fields.insert(
            "decorators".to_string(),
            QueryValue::List(
                fn_val
                    .decorators
                    .iter()
                    .map(|d| QueryValue::String(d.clone()))
                    .collect(),
            ),
        );

        // ── Reverse-index enrichment ──────────────────────────────────
        fields.insert(
            "caller_count".to_string(),
            QueryValue::Int(
                snapshot
                    .callers_by_callee
                    .get(fn_val.id.as_str())
                    .map(|s| s.len() as i64)
                    .unwrap_or(0),
            ),
        );
        fields.insert(
            "callee_count".to_string(),
            QueryValue::Int(
                snapshot
                    .callees_by_caller
                    .get(fn_val.id.as_str())
                    .map(|s| s.len() as i64)
                    .unwrap_or(0),
            ),
        );
        fields.insert(
            "callers".to_string(),
            QueryValue::List(
                snapshot
                    .callers_by_callee
                    .get(fn_val.id.as_str())
                    .map(|s| s.iter().map(|id| QueryValue::String(id.clone())).collect())
                    .unwrap_or_default(),
            ),
        );
        fields.insert(
            "callees".to_string(),
            QueryValue::List(
                snapshot
                    .callees_by_caller
                    .get(fn_val.id.as_str())
                    .map(|s| s.iter().map(|id| QueryValue::String(id.clone())).collect())
                    .unwrap_or_default(),
            ),
        );

        // Resolved calls as entity IDs
        fields.insert(
            "resolved_call_targets".to_string(),
            QueryValue::List(
                fn_val
                    .resolved_calls
                    .iter()
                    .filter_map(|rc| match rc {
                        ResolvedCall::Function(id) | ResolvedCall::Method { method: id, .. } | ResolvedCall::Constructor(id) => {
                            Some(QueryValue::String(id.clone()))
                        }
                        ResolvedCall::External(s) => Some(QueryValue::String(s.clone())),
                        ResolvedCall::Builtin(s) => Some(QueryValue::String(s.clone())),
                        _ => None,
                    })
                    .collect(),
            ),
        );
        fields.insert(
            "parameter_count".to_string(),
            QueryValue::Int(fn_val.parameters.len() as i64),
        );
        if let Some(ref rt) = fn_val.return_type {
            fields.insert("return_type".to_string(), QueryValue::String(rt.clone()));
        }

        // Evaluate WHERE clause
        if let Some(pred) = &query.where_clause {
            if !evaluate_predicate(pred, &fields) {
                continue;
            }
        }

        // Apply SELECT projection
        let projected = if query.select.is_empty() {
            fields
        } else {
            project_fields(&fields, &query.select)
        };

        rows.push(QueryRow {
            fields: projected,
        });
    }
    rows
}

fn scan_classes(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let mut rows = Vec::new();
    for (_id, cls) in snapshot.classes.iter() {
        // cls is Arc<Class>
        let mut fields = HashMap::new();
        fields.insert("name".to_string(), QueryValue::String(cls.name.clone()));
        fields.insert(
            "line".to_string(),
            QueryValue::Int(cls.line as i64),
        );
        fields.insert(
            "method_count".to_string(),
            QueryValue::Int(cls.methods.len() as i64),
        );
        fields.insert(
            "decorators".to_string(),
            QueryValue::List(
                cls.decorators
                    .iter()
                    .map(|d| QueryValue::String(d.clone()))
                    .collect(),
            ),
        );

        if let Some(pred) = &query.where_clause {
            if !evaluate_predicate(pred, &fields) {
                continue;
            }
        }

        let projected = if query.select.is_empty() {
            fields
        } else {
            project_fields(&fields, &query.select)
        };
        rows.push(QueryRow {
            fields: projected,
        });
    }
    rows
}

fn scan_modules(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let mut rows = Vec::new();
    for (_id, module) in snapshot.modules.iter() {
        let mut fields = HashMap::new();
        fields.insert("name".into(), QueryValue::String(module.name.clone()));
        fields.insert("path".into(), QueryValue::String(module.path.to_string_lossy().to_string()));
        fields.insert("language".into(), QueryValue::String(format!("{:?}", module.language)));
        fields.insert("class_count".into(), QueryValue::Int(module.classes.len() as i64));
        fields.insert("function_count".into(), QueryValue::Int(module.functions.len() as i64));
        fields.insert("import_count".into(), QueryValue::Int(module.imports.len() as i64));

        if let Some(pred) = &query.where_clause {
            if !evaluate_predicate(pred, &fields) {
                continue;
            }
        }
        let projected = if query.select.is_empty() { fields } else { project_fields(&fields, &query.select) };
        rows.push(QueryRow { fields: projected });
    }
    rows
}

fn scan_imports(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let mut rows = Vec::new();
    for (_id, import) in snapshot.imports.iter() {
        let mut fields = HashMap::new();
        fields.insert("raw".into(), QueryValue::String(import.raw.clone()));
        fields.insert("kind".into(), QueryValue::String(format!("{:?}", import.kind)));
        fields.insert("line".into(), QueryValue::Int(import.line as i64));
        fields.insert("is_type_only".into(), QueryValue::Bool(import.is_type_only));

        // Resolved target
        match &import.resolution {
            ImportResolution::Module(id) | ImportResolution::Symbol(SymbolId::Module(id)) => {
                fields.insert("resolved_module".into(), QueryValue::String(id.clone()));
            }
            ImportResolution::Symbol(SymbolId::Function(id)) | ImportResolution::Symbol(SymbolId::Class(id)) | ImportResolution::Symbol(SymbolId::Import(id)) => {
                fields.insert("resolved_target".into(), QueryValue::String(id.clone()));
            }
            ImportResolution::External { distribution } => {
                fields.insert("resolved_target".into(), QueryValue::String(
                    distribution.clone().unwrap_or_else(|| "external".into())
                ));
            }
            _ => {}
        }

        if let Some(pred) = &query.where_clause {
            if !evaluate_predicate(pred, &fields) {
                continue;
            }
        }
        let projected = if query.select.is_empty() { fields } else { project_fields(&fields, &query.select) };
        rows.push(QueryRow { fields: projected });
    }
    rows
}

fn scan_calls(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let mut rows = Vec::new();
    // Produce one row per call edge from the callers_by_callee reverse index
    for (source_id, callees) in snapshot.callees_by_caller.iter() {
        for target_id in callees.iter() {
            let mut fields = HashMap::new();
            fields.insert("source".into(), QueryValue::String(source_id.clone()));
            fields.insert("target".into(), QueryValue::String(target_id.clone()));

            // Look up target kind
            if let Some(_fn) = snapshot.functions.get(target_id) {
                fields.insert("target_kind".into(), QueryValue::String("function".into()));
            } else if let Some(_cls) = snapshot.classes.get(target_id) {
                fields.insert("target_kind".into(), QueryValue::String("class".into()));
            } else {
                fields.insert("target_kind".into(), QueryValue::String("external".into()));
            }

            if let Some(pred) = &query.where_clause {
                if !evaluate_predicate(pred, &fields) {
                    continue;
                }
            }
            let projected = if query.select.is_empty() { fields } else { project_fields(&fields, &query.select) };
            rows.push(QueryRow { fields: projected });
        }
    }
    rows
}

fn scan_fields(snapshot: &ProjectedGraph, query: &ParsedQuery) -> Vec<QueryRow> {
    let mut rows = Vec::new();
    // Fields live on classes — iterate classes and emit each field as a row
    for (_class_id, cls) in snapshot.classes.iter() {
        for field in cls.fields.iter() {
            let mut fields = HashMap::new();
            fields.insert("name".into(), QueryValue::String(field.name.clone()));
            fields.insert("parent_class".into(), QueryValue::String(cls.id.clone()));
            if let Some(ref ann) = field.annotation {
                fields.insert("type_annotation".into(), QueryValue::String(ann.clone()));
            }
            fields.insert("is_class_var".into(), QueryValue::Bool(field.is_class_var));

            if let Some(pred) = &query.where_clause {
                if !evaluate_predicate(pred, &fields) {
                    continue;
                }
            }
            let projected = if query.select.is_empty() { fields } else { project_fields(&fields, &query.select) };
            rows.push(QueryRow { fields: projected });
        }
    }
    rows
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
        Predicate::And(a, b) => {
            evaluate_predicate(a, fields) && evaluate_predicate(b, fields)
        }
        Predicate::Or(a, b) => {
            evaluate_predicate(a, fields) || evaluate_predicate(b, fields)
        }
    }
}

fn resolve_operand(op: &Operand, fields: &HashMap<String, QueryValue>) -> QueryValue {
    match op {
        Operand::Path(parts) => {
            let name = parts.join(".");
            fields
                .get(&name)
                .cloned()
                .unwrap_or(QueryValue::Null)
        }
        Operand::StringValue(s) => QueryValue::String(s.clone()),
        Operand::NumberValue(n) => QueryValue::Float(*n),
        Operand::BoolValue(b) => QueryValue::Bool(*b),
        Operand::ListValue(items) => QueryValue::List(
            items.iter().map(|i| resolve_operand(i, fields)).collect(),
        ),
        Operand::DerivedCall { name, args } => {
            evaluate_derived_call(name, args, fields)
        }
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
            let target = args.first().map(|a| operand_to_string(a)).unwrap_or_default();
            let parent = fields.get("parent_class")
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
                let found = items.iter().any(|item| value_to_string(item).contains(&search));
                QueryValue::Bool(found)
            } else if let Some(val) = fields.get(&list_field) {
                QueryValue::Bool(value_to_string(val).contains(&search))
            } else {
                QueryValue::Bool(false)
            }
        }
        "has_method" => {
            let target = args.first().map(|a| operand_to_string(a)).unwrap_or_default();
            let methods = fields.get("method_names")
                .or_else(|| fields.get("methods"));
            match methods {
                Some(QueryValue::List(items)) => {
                    QueryValue::Bool(items.iter().any(|item| value_to_string(item) == target))
                }
                _ => QueryValue::Bool(false),
            }
        }
        "overrides_of" => {
            // Present when parent_class is set AND the method exists on both
            let parent = fields.get("parent_class")
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
            SelectItem::Aggregate { func: _, path: _, alias } => {
                // Aggregates computed during group-by phase
                result.insert(alias.clone(), QueryValue::Int(0));
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
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> { slf }

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
        Ok(Some(row.into_pyobject(py)))
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
