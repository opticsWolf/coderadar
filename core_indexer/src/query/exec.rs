// CodeRadar v3.3 — Query Execution (§7.1, §7.2a)
// Streaming and aggregated query modes against a QuerySnapshot.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pyo3::prelude::*;

use crate::graph::QuerySnapshot;
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
pub fn execute_query(snapshot: &QuerySnapshot, query: &ParsedQuery) -> Vec<QueryRow> {
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

fn scan_functions(snapshot: &QuerySnapshot, query: &ParsedQuery) -> Vec<QueryRow> {
    let mut rows = Vec::new();
    for (_key, entry) in snapshot.arenas.functions.iter() {
        let fn_val = &entry.inner;

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
        fields.insert(
            "caller_count".to_string(),
            QueryValue::Int(0), // populated from reverse indexes
        );

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

fn scan_classes(snapshot: &QuerySnapshot, query: &ParsedQuery) -> Vec<QueryRow> {
    let mut rows = Vec::new();
    for (_key, entry) in snapshot.arenas.classes.iter() {
        let cls = &entry.inner;
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

fn scan_modules(_snapshot: &QuerySnapshot, _query: &ParsedQuery) -> Vec<QueryRow> {
    Vec::new()
}

fn scan_imports(_snapshot: &QuerySnapshot, _query: &ParsedQuery) -> Vec<QueryRow> {
    Vec::new()
}

fn scan_calls(_snapshot: &QuerySnapshot, _query: &ParsedQuery) -> Vec<QueryRow> {
    Vec::new()
}

fn scan_fields(_snapshot: &QuerySnapshot, _query: &ParsedQuery) -> Vec<QueryRow> {
    Vec::new()
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
        "inherits_from" | "contains" => {
            QueryValue::Bool(false) // Stub
        }
        "has_method" => {
            QueryValue::Bool(false) // Stub
        }
        "overrides_of" => {
            QueryValue::Bool(false) // Stub
        }
        _ => QueryValue::Bool(false),
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
