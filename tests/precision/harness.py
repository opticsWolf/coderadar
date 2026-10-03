"""Call-graph precision benchmark (v0.10 plan, Phase 7.1-7.2).

Two questions, answered against a real project:

* **Extraction recall** - of the calls Python's own ``ast`` sees inside
  functions, how many did the indexer extract at all? Broken down by call
  shape (``f()``, ``self.m()``, ``var.m()``, ``a.b.m()``, ``f().m()``).
* **Resolution** - of the calls the indexer extracted, how many were bound to
  the right in-repo entity? Ground truth is the ``# -> Target`` annotations in
  the fixture project (``Target`` is a qualified name, ``external`` or
  ``builtin``).

The numbers are the product; `baseline.json` ratchets them so a change that
loses recall or precision fails CI instead of shipping.
"""

from __future__ import annotations

import ast
import collections
import os
import re
from contextlib import contextmanager
from pathlib import Path

import coderadar
from coderadar import _core

SHAPES = ("name", "self.m", "var.m", "a.b.m", "f().m", "other")
_ANNOTATION = re.compile(r"#\s*->\s*(\S+)")
_RESOLVED = {"function", "method", "constructor"}


@contextmanager
def _cwd(path: Path):
    old = os.getcwd()
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(old)


def _shape(func: ast.expr) -> tuple[str, int]:
    """Classify a call's callee; returns (shape, line the callee name sits on)."""
    if isinstance(func, ast.Name):
        return "name", func.lineno
    if isinstance(func, ast.Attribute):
        line = func.end_lineno or func.lineno
        v = func.value
        if isinstance(v, ast.Name):
            return ("self.m" if v.id in ("self", "cls") else "var.m"), line
        if isinstance(v, ast.Attribute):
            return "a.b.m", line
        if isinstance(v, ast.Call):
            return "f().m", line
        return "other", line
    return "other", getattr(func, "lineno", 0)


def _callee_name(func: ast.expr) -> str | None:
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute):
        return func.attr
    return None


def _calls_in_functions(tree: ast.AST):
    """Yield (name, shape, line) for every call lexically inside a def."""
    def walk(node, in_def):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            # Decorators, defaults and annotations run in the enclosing scope.
            for part in (*node.decorator_list, node.args, node.returns):
                if part is not None:
                    yield from visit(part, in_def)
            for stmt in node.body:
                yield from visit(stmt, True)
            return
        for child in ast.iter_child_nodes(node):
            yield from visit(child, in_def)

    def visit(node, in_def):
        if isinstance(node, ast.Call) and in_def:
            shape, line = _shape(node.func)
            yield _callee_name(node.func), shape, line
        yield from walk(node, in_def)

    yield from visit(tree, False)


def _function_ids(file_rel: str) -> list[str]:
    kids = _core.module_children(f"{file_rel}::module") or {}
    return [f["id"] for f in kids.get("functions", [])]


def _graph_sites(file_rel: str) -> list[dict]:
    sites = []
    for fid in _function_ids(file_rel):
        sites.extend(_core.call_sites(fid) or [])
    return sites


def measure(root: Path) -> dict:
    """Index ``root`` and return extraction + resolution metrics."""
    root = Path(root).resolve()
    with _cwd(root):
        graph = coderadar.analyze(str(root))
        mods = [m["path"] for m in graph.query("modules") if m["path"].endswith(".py")]

        shape_total = collections.Counter()
        shape_found = collections.Counter()
        status = collections.Counter()
        res = collections.Counter()  # tp / wrong / missed
        mismatches: list[str] = []

        for rel in sorted(mods):
            src = (root / rel).read_bytes().decode("utf-8", errors="replace")
            try:
                tree = ast.parse(src)
            except SyntaxError:
                continue
            truth = collections.Counter()
            kind_of = {}
            for name, shape, line in _calls_in_functions(tree):
                if name is None:  # `x[0]()` / `f()()`: no name to extract
                    continue
                truth[(line, name)] += 1
                kind_of.setdefault((line, name), []).append(shape)
            sites = _graph_sites(rel)
            got = collections.Counter((s["line"], s["name"]) for s in sites)
            for key, n in truth.items():
                hit = min(n, got.get(key, 0))
                shapes = kind_of[key]
                for i, shape in enumerate(shapes):
                    shape_total[shape] += 1
                    if i < hit:
                        shape_found[shape] += 1
                    else:
                        mismatches.append(f"{rel}:{key[0]} {shape} {key[1]}")
            for s in sites:
                status[s["status"]] += 1

            expected = {}
            for i, text in enumerate(src.splitlines(), 1):
                m = _ANNOTATION.search(text)
                if m and m.group(1) != "?":
                    expected[i] = m.group(1)
            by_line = collections.defaultdict(list)
            for s in sites:
                by_line[s["line"]].append(s)
            for line, want in expected.items():
                cands = by_line.get(line, [])
                if want not in ("external", "builtin"):
                    # Several calls can share a line; judge the one named in the arrow.
                    leaf = want.rsplit(".", 1)[-1]
                    cands = [s for s in cands if s["name"] == leaf] or cands
                ok_site = next((s for s in cands if _matches(s, want)), None)
                if ok_site is not None:
                    res["tp"] += 1
                    continue
                bound_in_repo = any(s["status"] in _RESOLVED for s in cands)
                if bound_in_repo:
                    res["wrong"] += 1
                    mismatches.append(f"{rel}:{line} wrong target, want {want}")
                elif want in ("external", "builtin"):
                    res["tp"] += 1  # not bound to the repo: right for these
                else:
                    res["missed"] += 1
                    mismatches.append(f"{rel}:{line} unresolved, want {want}")

    total = sum(shape_total.values())
    found = sum(shape_found.values())
    tp, wrong, missed = res["tp"], res["wrong"], res["missed"]
    sited = sum(status.values())
    return {
        "calls": total,
        "extraction_recall": found / total if total else 1.0,
        "by_shape": {
            s: {"total": shape_total[s], "found": shape_found[s]}
            for s in SHAPES if shape_total[s]
        },
        "status": dict(status),
        "in_repo_rate": sum(status[k] for k in _RESOLVED) / sited if sited else 0.0,
        "annotated": tp + wrong + missed,
        "resolution_precision": tp / (tp + wrong) if tp + wrong else 1.0,
        "resolution_recall": tp / (tp + wrong + missed) if tp + wrong + missed else 1.0,
        "mismatches": mismatches,
    }


def _matches(site: dict, want: str) -> bool:
    if want in ("external", "builtin"):
        return site["status"] == want
    if site["status"] not in _RESOLVED or not site["target"]:
        return False
    return site["target"].split("::", 1)[-1] == want


def dangling_targets(root: Path) -> list[str]:
    """Resolved call targets that name no entity in the index (must be empty)."""
    root = Path(root).resolve()
    with _cwd(root):
        graph = coderadar.analyze(str(root))
        mods = [m["path"] for m in graph.query("modules")]
        known, sites = set(), []
        for rel in mods:
            kids = _core.module_children(f"{rel}::module") or {}
            known.update(f["id"] for f in kids.get("functions", []))
            known.update(c["id"] for c in kids.get("classes", []))
            known.add(f"{rel}::module")
            for fid in _function_ids(rel):
                sites.extend((fid, s) for s in _core.call_sites(fid) or [])
    return [f"{fid} -> {s['target']}" for fid, s in sites
            if s["status"] in _RESOLVED and s["target"] not in known]


# ── Dead-code precision (§7.2, fourth metric) ─────────────────────────────
#
# "Dead-code precision = sample 50 High+Medium findings, hand-labelled once,
# stored as a golden file." The sample below is that golden: every `alive`
# entry was a High finding at some point during the 0.10 work and was
# verified by hand to be reachable (public API, registration decorator,
# aliased import, value reference); every `dead` entry was verified to have
# no call site anywhere in the corpus. Precision is a gate, recall is a
# ratchet — the finding budget may shrink, never grow.

_HIGH_MEDIUM = {"High", "Medium"}


def dead_code_report(root: Path, golden: dict) -> dict:
    root = Path(root).resolve()
    with _cwd(root):
        coderadar.analyze(str(root))
        findings = _core.find_dead_code(0.0, False, 2000)

    strong = {f["entity_id"] for f in findings if f["tier"] in _HIGH_MEDIUM}
    reported = {f["entity_id"] for f in findings}
    false_positives = [e for e in golden["alive"] if e in strong]
    missed = [e for e in golden["dead"] if e not in reported]
    labelled = len(golden["alive"]) + len(golden["dead"])
    return {
        "findings": len(findings),
        "high_medium_findings": len(strong),
        "labelled": labelled,
        "precision": (labelled - len(false_positives)) / labelled if labelled else 1.0,
        "recall": (len(golden["dead"]) - len(missed)) / len(golden["dead"])
        if golden["dead"]
        else 1.0,
        "false_positives": false_positives,
        "missed": missed,
    }


def format_dead_report(name: str, m: dict) -> str:
    return (
        f"{name}: {m['findings']} dead-code findings, "
        f"{m['high_medium_findings']} High+Medium, "
        f"labelled precision {m['precision']:.1%}, recall {m['recall']:.1%}"
    )


def format_report(name: str, m: dict) -> str:
    lines = [
        (
            f"{name}: {m['calls']} calls, extraction recall {m['extraction_recall']:.1%}, "
            f"in-repo rate {m['in_repo_rate']:.1%}"
        )
    ]
    for s, v in m["by_shape"].items():
        lines.append(f"  {s:8} {v['found']:5}/{v['total']:<5}")
    if m["annotated"]:
        lines.append(
            f"  annotated {m['annotated']}: precision {m['resolution_precision']:.1%}, "
            f"recall {m['resolution_recall']:.1%}"
        )
    return "\n".join(lines)
