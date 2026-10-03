//! Receiver typing for call resolution (v0.10 plan 1.2).
//!
//! A cheap, flow-insensitive type environment. It answers one question:
//! "which in-repo class does this expression evaluate to?" from constructor
//! calls, annotations, `return Ctor()` / `return local.attr`, instance-attribute
//! assignments and pytest fixtures. Evidence must agree: one piece that names
//! no class makes the whole name unknown, so the resolver leaves the call
//! unresolved rather than guess. Depth-limited so factories calling factories
//! (or recursion) terminate.

use super::module_resolution::{find_module_by_dotted_name, find_symbol_in_module};
use crate::types::*;
use std::collections::HashMap;

/// `class id -> [(method name, method id)]`, in the order the projection's
/// function map yields them.
pub(super) type MethodsByClass = HashMap<EntityId, Vec<(String, String)>>;

const MAX_DEPTH: usize = 5;
const RETURN: &str = "<return>";
const YIELD: &str = "<yield>";

pub(super) struct TypeCtx<'a> {
    pub projection: &'a ProjectedGraph,
    pub methods_by_class: &'a MethodsByClass,
}

/// All evidence must agree on one class.
fn unique(v: Vec<Option<EntityId>>) -> Option<EntityId> {
    let mut v = v.into_iter().collect::<Option<Vec<_>>>()?;
    v.sort();
    v.dedup();
    (v.len() == 1).then(|| v.remove(0))
}

fn split_dotted(s: &str) -> (Vec<String>, &str) {
    match s.rsplit_once('.') {
        Some((p, n)) => (p.split('.').map(String::from).collect(), n),
        None => (vec![], s),
    }
}

impl<'a> TypeCtx<'a> {
    fn func(&self, id: &str) -> Option<&'a Function> {
        self.projection.functions.get(id).map(|f| f.as_ref())
    }

    /// `name` on `class_id` or anything in its MRO.
    pub fn method_of(&self, class_id: &str, name: &str) -> Option<EntityId> {
        let find = |cid: &str| {
            self.methods_by_class
                .get(cid)
                .and_then(|ms| ms.iter().find(|(n, _)| n == name).map(|(_, id)| id.clone()))
        };
        find(class_id).or_else(|| {
            self.projection.classes.get(class_id)?.mro.iter().find_map(|node| match node {
                MroNode::Class(cid) => find(cid),
                _ => None,
            })
        })
    }

    /// A class named in (or imported into) module `scope`.
    pub fn class_in_scope(&self, scope: &str, name: &str) -> Option<EntityId> {
        let id = find_symbol_in_module(self.projection, scope, name)?;
        self.projection.classes.contains_key(&id).then_some(id)
    }

    /// `Foo` or `pkg.Foo` read in `scope`.
    pub fn class_of_ref(&self, scope: &str, path: &[String], name: &str) -> Option<EntityId> {
        if path.is_empty() {
            return self.class_in_scope(scope, name);
        }
        let mid = find_module_by_dotted_name(self.projection, &path.join("."), scope)?;
        let id = find_symbol_in_module(self.projection, &mid, name)?;
        self.projection.classes.contains_key(&id).then_some(id)
    }

    /// `Foo`, `"Foo"`, `Optional[Foo]`, `Foo | None`, `pkg.Foo`.
    pub fn class_of_annotation(&self, ann: &str, scope: &str) -> Option<EntityId> {
        let mut a = ann.trim().trim_matches(|c| c == '"' || c == '\'').trim();
        if let Some(inner) = a.strip_prefix("Optional[").and_then(|s| s.strip_suffix(']')) {
            a = inner.trim();
        }
        let a = a.split('|').map(str::trim).find(|p| *p != "None")?;
        if a.is_empty() || !a.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
            return None;
        }
        let (path, name) = split_dotted(a);
        self.class_of_ref(scope, &path, name)
    }

    /// The class a binding's right-hand side (or annotation) evaluates to,
    /// read inside function `f`.
    fn type_of_binding(&self, f: &Function, b: &Binding, d: usize) -> Option<EntityId> {
        if let Some(c) = b.annotation.as_deref().and_then(|a| self.class_of_annotation(a, &f.parent_module)) {
            return Some(c);
        }
        if let Some(r) = &b.rhs {
            return self.call_result(f, &r.path, &r.name, d + 1);
        }
        if let Some(path) = &b.expr {
            return self.type_of_path(f, path, d + 1);
        }
        None
    }

    /// What calling `path.name(...)` inside `f` produces.
    fn call_result(&self, f: &Function, path: &[String], name: &str, d: usize) -> Option<EntityId> {
        if d > MAX_DEPTH {
            return None;
        }
        if let Some(c) = self.class_of_ref(&f.parent_module, path, name) {
            return Some(c);
        }
        // `make_desk(...)` where `make_desk` is an unannotated parameter: a
        // pytest fixture that hands back a callable.
        if path.is_empty() && self.is_untyped_param(f, name) {
            let fx = self.fixture(f, name)?;
            return unique(
                self.value_bindings(fx)
                    .map(|b| {
                        let target = b.expr.as_ref().filter(|e| e.len() == 1)?;
                        let inner = self.func(&format!("{}.{}", fx.id, target[0]))?;
                        self.function_result(inner, d + 1)
                    })
                    .collect(),
            );
        }
        let fid = if path.is_empty() {
            find_symbol_in_module(self.projection, &f.parent_module, name)?
        } else {
            let mid = find_module_by_dotted_name(self.projection, &path.join("."), &f.parent_module)?;
            find_symbol_in_module(self.projection, &mid, name)?
        };
        let callee = self.func(&fid)?;
        if callee.parent_class.is_some() {
            return None;
        }
        self.function_result(callee, d + 1)
    }

    /// What a call to function `callee` returns: its annotation, else its
    /// `return` expressions (all must agree).
    fn function_result(&self, callee: &Function, d: usize) -> Option<EntityId> {
        if d > MAX_DEPTH {
            return None;
        }
        if let Some(c) = callee
            .return_type
            .as_deref()
            .and_then(|a| self.class_of_annotation(a, &callee.parent_module))
        {
            return Some(c);
        }
        unique(
            callee
                .bindings
                .iter()
                .filter(|b| b.target.len() == 1 && b.target[0] == RETURN)
                .map(|b| self.type_of_binding(callee, b, d))
                .collect(),
        )
    }

    fn is_untyped_param(&self, f: &Function, name: &str) -> bool {
        f.parameters.iter().any(|p| p.name == name && p.annotation.is_none())
    }

    fn value_bindings<'b>(&self, fx: &'b Function) -> impl Iterator<Item = &'b Binding> {
        fx.bindings
            .iter()
            .filter(|b| b.target.len() == 1 && (b.target[0] == RETURN || b.target[0] == YIELD))
    }

    /// The pytest fixture `name` visible from `f`: its own module, then
    /// `conftest.py` in the same and each parent directory.
    fn fixture(&self, f: &Function, name: &str) -> Option<&'a Function> {
        let is_fixture = |g: &Function| g.name == name && g.parent_class.is_none() && g.decorators.iter().any(|d| d.contains("fixture"));
        let in_module = |module_id: &str| -> Option<&'a Function> {
            let m = self.projection.modules.get(module_id)?;
            m.functions.iter().filter_map(|id| self.func(id)).find(|g| is_fixture(g))
        };
        if let Some(g) = in_module(&f.parent_module) {
            return Some(g);
        }
        let path = f.parent_module.rsplit_once("::").map(|(p, _)| p)?;
        let sep = if path.contains('\\') { '\\' } else { '/' };
        let mut dir = path.rsplit_once(sep).map(|(d, _)| d.to_string())?;
        loop {
            if let Some(g) = in_module(&format!("{dir}{sep}conftest.py::module")) {
                return Some(g);
            }
            match dir.rsplit_once(sep) {
                Some((parent, _)) => dir = parent.to_string(),
                None => return in_module("conftest.py::module"),
            }
        }
    }

    /// Type of the bare name `name` inside `f`.
    fn local_type(&self, f: &Function, name: &str, d: usize) -> Option<EntityId> {
        if d > MAX_DEPTH {
            return None;
        }
        let mut found = Vec::new();
        for p in f.parameters.iter().filter(|p| p.name == name) {
            if let Some(a) = p.annotation.as_deref() {
                found.push(self.class_of_annotation(a, &f.parent_module));
            }
        }
        for b in f.bindings.iter().filter(|b| b.target.len() == 1 && b.target[0] == name) {
            found.push(self.type_of_binding(f, b, d));
        }
        if found.is_empty() && self.is_untyped_param(f, name) {
            // A fixture's value: every `return` / `yield` in it must agree.
            let fx = self.fixture(f, name)?;
            found = self.value_bindings(fx).map(|b| self.type_of_binding(fx, b, d)).collect();
        }
        unique(found)
    }

    /// Type of `self.<attr>` (or `obj.<attr>`) on a class: assignments in its
    /// methods and annotated class-level fields, each read in the scope of the
    /// module that wrote it.
    fn attr_type(&self, class_id: &str, attr: &str, d: usize) -> Option<EntityId> {
        if d > MAX_DEPTH {
            return None;
        }
        let mut scan = vec![class_id.to_string()];
        if let Some(c) = self.projection.classes.get(class_id) {
            scan.extend(c.mro.iter().filter_map(|n| match n {
                MroNode::Class(cid) => Some(cid.clone()),
                _ => None,
            }));
        }
        let mut found = Vec::new();
        for cid in &scan {
            if let Some(c) = self.projection.classes.get(cid) {
                for fld in c.fields.iter().filter(|f| f.name == attr) {
                    if let Some(a) = fld.annotation.as_deref() {
                        found.push(self.class_of_annotation(a, &c.parent_module));
                    }
                }
            }
            for (_, fid) in self.methods_by_class.get(cid).into_iter().flatten() {
                let Some(m) = self.func(fid) else { continue };
                for b in m.bindings.iter().filter(|b| b.target.len() == 2 && b.target[1] == attr) {
                    found.push(self.type_of_binding(m, b, d));
                }
            }
        }
        unique(found)
    }

    /// Type of a receiver path read inside `f`: `["self","ser"]`, `["m"]`,
    /// `["<call:make>"]`, `["desk","manager"]`.
    pub fn type_of_path(&self, f: &Function, path: &[String], d: usize) -> Option<EntityId> {
        if d > MAX_DEPTH {
            return None;
        }
        let first = path.first()?;
        let mut t = if first == "self" || first == "cls" {
            f.parent_class.clone()?
        } else if let Some(call) = first.strip_prefix("<call:").and_then(|s| s.strip_suffix('>')) {
            let (p, n) = split_dotted(call);
            self.call_result(f, &p, n, d + 1)?
        } else {
            self.local_type(f, first, d)?
        };
        for seg in &path[1..] {
            t = self.attr_type(&t, seg, d + 1)?;
        }
        Some(t)
    }

    /// The function a *reference* `path.name` (a callback passed as a value,
    /// not called) made inside function `fid` points at.
    pub fn resolve_ref(&self, fid: &str, path: &[String], name: &str) -> Option<EntityId> {
        let f = self.func(fid)?;
        let is_plain_callable = |id: &str| {
            self.func(id).is_some_and(|g| {
                !g.decorators.iter().any(|d| d.contains("property") || d.contains(".setter"))
            })
        };
        let target = if path.is_empty() {
            // A parameter or local of that name shadows any function.
            let shadows = |g: &Function| {
                g.parameters.iter().any(|p| p.name == name)
                    || g.bindings.iter().any(|b| b.target.len() == 1 && b.target[0] == name)
            };
            if shadows(f) {
                return None;
            }
            // ...including those of the functions this one is nested in.
            let mut outer = f.id.as_str();
            while let Some((head, _)) = outer.rsplit_once('.') {
                if self.func(head).is_some_and(shadows) {
                    return None;
                }
                outer = head;
            }
            let nested = format!("{}.{}", f.id, name);
            if self.projection.functions.contains_key(&nested) {
                Some(nested)
            } else {
                find_symbol_in_module(self.projection, &f.parent_module, name)
                    .filter(|id| self.func(id).is_some_and(|g| g.parent_class.is_none()))
            }
        } else if let Some(t) = self.type_of_path(f, path, 0) {
            self.method_of(&t, name)
        } else if let Some(c) = self.class_of_ref(&f.parent_module, &path[..path.len() - 1], &path[path.len() - 1]) {
            self.method_of(&c, name)
        } else {
            find_module_by_dotted_name(self.projection, &path.join("."), &f.parent_module)
                .and_then(|m| find_symbol_in_module(self.projection, &m, name))
                .filter(|id| self.func(id).is_some_and(|g| g.parent_class.is_none()))
        };
        target.filter(|id| is_plain_callable(id) && id != fid)
    }

    /// The method a call `path.name(...)` made inside function `fid` binds to.
    pub fn resolve_method_call(&self, fid: &str, path: &[String], name: &str) -> Option<EntityId> {
        let f = self.func(fid)?;
        let t = self.type_of_path(f, path, 0)?;
        self.method_of(&t, name)
    }
}
