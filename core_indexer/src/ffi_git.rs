// Split out of lib.rs (DR-28, P4 hygiene). Move-only: no behavior change.

use pyo3::prelude::*;
use pyo3::types::PyDict;

/// `{"clean": bool}` — `git status` semantics: modified and untracked
/// files make the tree dirty, ignored files never do. Reports clean when
/// git cannot tell.
#[pyfunction]
pub fn git_worktree_clean(py: Python<'_>, repo_path: &str) -> PyResult<PyObject> {
    let clean = crate::fs::git::is_worktree_clean(repo_path).unwrap_or(true);
    let dict = PyDict::new(py);
    dict.set_item("clean", clean)?;
    Ok(dict.into())
}

/// Blame for one file as run-length rows `{line, count, author, commit}`:
/// `count` consecutive lines starting at `line` share an author and commit.
#[pyfunction]
pub fn git_blame(py: Python<'_>, repo_path: &str, file_path: &str) -> PyResult<Vec<PyObject>> {
    match crate::fs::git::blame_file(repo_path, file_path) {
        Ok(lines) => {
            let rows: Vec<PyObject> = lines
                .iter()
                .map(|l| {
                    let d = PyDict::new(py);
                    let _ = d.set_item("line", l.line_number);
                    let _ = d.set_item("count", l.line_count);
                    let _ = d.set_item("author", &l.author);
                    let _ = d.set_item("commit", &l.commit);
                    d.into()
                })
                .collect();
            Ok(rows)
        }
        Err(e) => Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
            "git blame failed: {:?}",
            e
        ))),
    }
}

/// Repo-relative paths changed between two committed revisions (any
/// rev syntax: HEAD~1, branch, tag, oid). `new_oid=None` means HEAD.
/// Raises `RuntimeError` on an unknown revision.
#[pyfunction]
pub fn git_changed_files(
    _py: Python<'_>,
    repo_path: &str,
    old_oid: Option<&str>,
    new_oid: Option<&str>,
) -> PyResult<Vec<String>> {
    // R2-6: revision strings pass through unresolved; unknown ones
    // error (UnknownRevision) instead of diffing empty.
    match crate::fs::git::changed_files_between(repo_path, old_oid, new_oid) {
        Ok(files) => Ok(files),
        Err(e) => Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
            "git diff failed: {:?}",
            e
        ))),
    }
}
