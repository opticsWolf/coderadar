// Split out of lib.rs (DR-28, P4 hygiene). Move-only: no behavior change.

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyList};
use serde_json::Value;

fn py_to_value(value: &Bound<'_, PyAny>) -> PyResult<Value> {
    if value.is_none() {
        return Ok(Value::Null);
    }
    if let Ok(boolean) = value.downcast::<PyBool>() {
        return Ok(Value::Bool(boolean.extract()?));
    }
    if let Ok(text) = value.extract::<String>() {
        return Ok(Value::String(text));
    }
    if let Ok(number) = value.extract::<i64>() {
        return Ok(Value::Number(number.into()));
    }
    if let Ok(number) = value.extract::<f64>() {
        return serde_json::Number::from_f64(number)
            .map(Value::Number)
            .ok_or_else(|| pyo3::exceptions::PyTypeError::new_err("config number is not finite"));
    }
    if let Ok(list) = value.downcast::<PyList>() {
        let mut rows = Vec::with_capacity(list.len());
        for item in list.iter() {
            rows.push(py_to_value(&item)?);
        }
        return Ok(Value::Array(rows));
    }
    if let Ok(dict) = value.downcast::<PyDict>() {
        return dict_to_value(dict);
    }
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "config value of unsupported type '{}'",
        value.get_type().name()?
    )))
}

fn dict_to_value(dict: &Bound<'_, PyDict>) -> PyResult<Value> {
    let mut map = serde_json::Map::new();
    for (key, value) in dict.iter() {
        map.insert(key.extract::<String>()?, py_to_value(&value)?);
    }
    Ok(Value::Object(map))
}

fn value_to_py(py: Python<'_>, value: &Value) -> PyResult<PyObject> {
    match value {
        Value::Null => Ok(py.None()),
        Value::Bool(flag) => Ok(PyBool::new(py, *flag).to_owned().unbind().into()),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(value.into_pyobject(py)?.unbind().into())
            } else if let Some(value) = number.as_u64() {
                Ok(value.into_pyobject(py)?.unbind().into())
            } else {
                let value = number.as_f64().unwrap_or(0.0);
                Ok(value.into_pyobject(py)?.unbind().into())
            }
        }
        Value::String(text) => Ok(text.into_pyobject(py)?.unbind().into()),
        Value::Array(rows) => {
            let list = PyList::empty(py);
            for row in rows {
                list.append(value_to_py(py, row)?)?;
            }
            Ok(list.into_any().unbind())
        }
        Value::Object(map) => {
            let dict = PyDict::new(py);
            for (key, value) in map {
                dict.set_item(key, value_to_py(py, value)?)?;
            }
            Ok(dict.into_any().unbind())
        }
    }
}

/// Push a `.coderadar.toml`-shaped dict into the process configuration.
///
/// Python validates the schema; this function only translates the dict to a
/// JSON value and delegates mapping to `config_activation::apply_config`, the
/// same function the pure-Rust TOML path uses.
///
/// Returns `{"applied": {...}, "ignored": [...]}`. `ignored` names keys that
/// map to nothing, so an aspirational knob cannot appear to work.
#[pyfunction]
pub(crate) fn set_config(py: Python<'_>, cfg: &Bound<'_, PyDict>) -> PyResult<PyObject> {
    let raw = dict_to_value(cfg)?;
    let report = crate::config_activation::apply_config(&raw)
        .map_err(pyo3::exceptions::PyTypeError::new_err)?;

    let applied = PyDict::new(py);
    for (path, value) in report.applied {
        applied.set_item(path, value_to_py(py, &value)?)?;
    }
    let out = PyDict::new(py);
    out.set_item("applied", applied)?;
    out.set_item("ignored", report.ignored)?;
    Ok(out.into())
}

/// The configuration currently in force, as a dict.
#[pyfunction]
pub(crate) fn get_config(py: Python<'_>) -> PyResult<PyObject> {
    value_to_py(py, &crate::config_activation::active_json())
}
