"""v0.8 P1: ledger-backed cold start (design: docs/v0.8-p1-cold-start-design.md).

Three-way ingest parity (fossil-mcp-improvement-plan line 1244):

    analyze  ==  analyze + update_file  ==  analyze -> fresh process -> load_snapshot

Parity means the Python-facing entity dicts exposed by
``search_entities`` / ``graph_stats`` / ``index_edge_stats`` / ``traverse``,
not full in-memory ``ProjectedGraph`` equality. ``indexed_at`` and
``revision`` are excluded from the comparison: ``indexed_at`` is the wall
clock at analyze time vs. the store mtime at load time, and ``revision``
exists only on ledger-restored graphs (it is the Stage 0.3 cache key and is
asserted separately). ``indexed_root`` is excluded because ``analyze``
canonicalizes (Windows verbatim prefix) while ``load`` records the passed
path.

Each leg runs in a fresh subprocess because the graph is a process global.
"""
import json
import os
import shutil
import sqlite3
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent / "py_agent" / "src"))

import coderadar  # noqa: E402
from coderadar import cli as _cli  # noqa: E402

FIXTURE_DIR = Path(__file__).parent / "fixtures" / "cold_start"
SRC = Path(__file__).parent.parent / "py_agent" / "src"
KINDS = ["module", "class", "function", "import", "constant", "type_alias"]


def _copy_fixture(dest: Path) -> Path:
    shutil.copytree(FIXTURE_DIR, dest)
    return dest


def _downgrade_store_to_v1(db: Path) -> None:
    """Strip meta_version from the open concepts: a v1-style store.

    macrame's ``trg_concepts_monotonic_ra`` trigger requires
    ``recorded_at`` to strictly increase on every concept UPDATE, so
    each rewrite also bumps the row's timestamp (max existing + n µs).
    """
    import datetime

    fmt = "%Y-%m-%dT%H:%M:%S.%fZ"
    con = sqlite3.connect(str(db))
    try:
        rows = con.execute(
            "SELECT id, content, recorded_at FROM concepts WHERE retired = 0"
        ).fetchall()
        assert rows, "no open concepts in store"
        base = max(datetime.datetime.strptime(ra, fmt) for *_, ra in rows)
        for i, (cid, content, _) in enumerate(rows):
            data = json.loads(content)
            data.pop("meta_version", None)
            ts = (base + datetime.timedelta(microseconds=i + 1)).strftime(fmt)
            con.execute(
                "UPDATE concepts SET content = ?, recorded_at = ? "
                "WHERE id = ?",
                (json.dumps(data), ts, cid),
            )
        con.commit()
    finally:
        con.close()


def _open_concepts(db: Path):
    con = sqlite3.connect(str(db))
    try:
        return [json.loads(c) for (c,) in con.execute(
            "SELECT content FROM concepts WHERE retired = 0").fetchall()]
    finally:
        con.close()


# ── Subprocess leg script ─────────────────────────────────────────────────
LEG_SCRIPT = r"""
import json, sys
from pathlib import Path

src, mode, proj, out = sys.argv[1:5]
proj = Path(proj)
db = Path(sys.argv[5]) if len(sys.argv) > 5 else None
sys.path.insert(0, src)
import coderadar
from coderadar._core import (
    graph_stats, search_entities, index_edge_stats, traverse,
    register_synthetic_edges_bulk, update_file)

KINDS = ["module", "class", "function", "import", "constant", "type_alias"]

def synth():
    # Same deterministic framework edges on every leg: two functions from
    # different modules, both synthetic kinds.
    ids = {}
    for e in search_entities("", 1000, "function"):
        ids.setdefault(e["name"], e["id"])
    return [(ids["run"], ids["combine"], "CALLBACK"),
            (ids["combine"], ids["run"], "DEPENDS_ON")]

def dump():
    d = {"stats": graph_stats(), "counts": {}, "entities": {},
         "edges": index_edge_stats()}
    for k in KINDS:
        ents = search_entities("", 1000, k)
        d["counts"][k] = len(ents)
        d["entities"][k] = sorted(
            json.dumps(e, sort_keys=True, default=str) for e in ents)
    hub = [e["id"] for e in search_entities("", 1000, "module")
           if e["id"].endswith("main.py::module")]
    d["traverse"] = traverse(hub[0], 3,
                             ["CALLS", "IMPORTS", "EXTENDS", "OVERRIDES"],
                             "both", None)
    return d

if mode == "A":
    coderadar.analyze(str(proj), create_store=True)
    register_synthetic_edges_bulk(synth())
elif mode == "B":
    coderadar.analyze(str(proj), create_store=True)
    register_synthetic_edges_bulk(synth())
    main = proj / "main.py"
    # No-op content rewrite. The diff counters are not the contract here:
    # apply_diff_update always re-inserts the module unit, and main.py's
    # function reports a signature/body-hash mismatch against the analyze
    # pass (re-inserted under its stored id; entities_removed stays 0).
    # The contract is a clean apply with no removals - graph invariance is
    # asserted by the three-way comparison itself.
    res = update_file(str(main), main.read_text(encoding="utf-8"), None)
    assert res.get("fully_applied"), res
    assert res.get("entities_removed") == 0, res
else:  # C
    coderadar.load(str(db), str(proj))
Path(out).write_text(json.dumps(dump(), sort_keys=True, default=str))
"""


def _run_leg(mode: str, proj: Path, out: Path, db: Path = None) -> dict:
    args = [sys.executable, "-c", LEG_SCRIPT, str(SRC), mode,
            str(proj), str(out)]
    if db is not None:
        args.append(str(db))
    proc = subprocess.run(args, capture_output=True, text=True, timeout=600)
    assert proc.returncode == 0, (
        f"leg {mode} failed:\nstdout: {proc.stdout}\nstderr: {proc.stderr}")
    return json.loads(out.read_text(encoding="utf-8"))


def _norm(leg: dict) -> dict:
    leg = json.loads(json.dumps(leg))
    for key in ("indexed_at", "revision", "indexed_root"):
        leg["stats"].pop(key, None)
    return leg


def _default_db(proj: Path) -> Path:
    return proj / ".coderadar" / "store" / "coderadar.db"


# ── Three-way ingest parity ──────────────────────────────────────────────
def test_three_way_ingest_parity(tmp_path):
    # All three legs index the SAME directory: entity IDs embed the
    # absolute file path, so legs indexed from different directories would
    # differ by path prefix alone. Each leg still runs in a fresh process
    # because the global graph is process-global.
    proj = _copy_fixture(tmp_path / "proj")
    leg_a = _run_leg("A", proj, tmp_path / "a.json")
    # Leg C cold-starts from leg A's ledger (which also carries the
    # synthetic edges) BEFORE leg B re-indexes the same directory. The
    # store is a WAL-mode SQLite file, so it cannot be copied as bytes -
    # leg C must read the original path. Leg order is immaterial: every
    # leg is its own fresh process.
    leg_c = _run_leg("C", proj, tmp_path / "c.json",
                     db=_default_db(proj))
    leg_b = _run_leg("B", proj, tmp_path / "b.json")

    a, b, c = _norm(leg_a), _norm(leg_b), _norm(leg_c)
    assert a == b, "analyze != analyze + no-op update_file"
    assert a == c, "analyze != fresh-process load_snapshot"

    # The fixture actually exercises what it claims to.
    assert a["counts"]["module"] == 7
    assert a["counts"]["class"] == 2
    assert a["counts"]["function"] >= 9
    assert a["counts"]["import"] >= 8

    # Synthetic edges landed in the call indices on every leg.
    assert a["edges"]["callers_by_callee"] >= 10  # resolved calls + synthetic


def test_revision_is_the_stage_0_3_cache_key(tmp_path):
    proj_a = _copy_fixture(tmp_path / "a")
    leg_a = _run_leg("A", proj_a, tmp_path / "a.json")
    leg_c = _run_leg("C", proj_a, tmp_path / "c.json",
                     db=_default_db(proj_a))
    # analyze: no ledger revision was observed.
    assert leg_a["stats"].get("revision") is None
    # load: the seq anchor, usable as the analysis-cache key.
    assert isinstance(leg_c["stats"]["revision"], int)
    assert leg_c["stats"]["revision"] > 0


# ── v1 store rejection + fallback ────────────────────────────────────────
def test_v1_store_is_a_hard_error(tmp_path):
    proj = _copy_fixture(tmp_path / "proj")
    coderadar.analyze(str(proj), create_store=True)
    db = _default_db(proj)
    assert db.exists()
    _downgrade_store_to_v1(db)
    with pytest.raises(ValueError, match="meta_version"):
        coderadar.load(str(db), str(proj))


FALLBACK_SCRIPT = r"""
import json, sqlite3, sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
proj = Path(sys.argv[2]); db = proj / ".coderadar" / "store" / "coderadar.db"
import datetime
fmt = "%Y-%m-%dT%H:%M:%S.%fZ"
con = sqlite3.connect(str(db))
rows = con.execute(
    "SELECT id, content, recorded_at FROM concepts WHERE retired = 0").fetchall()
base = max(datetime.datetime.strptime(ra, fmt) for *_, ra in rows)
for i, (cid, content, _) in enumerate(rows):
    data = json.loads(content)
    data.pop("meta_version", None)
    ts = (base + datetime.timedelta(microseconds=i + 1)).strftime(fmt)
    con.execute("UPDATE concepts SET content = ?, recorded_at = ? "
                "WHERE id = ?", (json.dumps(data), ts, cid))
con.commit(); con.close()
from coderadar.cli import _ensure_graph
from coderadar._core import graph_stats
_ensure_graph(str(proj))
stats = graph_stats()
assert stats["modules"] == 7, stats
con2 = sqlite3.connect(str(db))
rows = [json.loads(c) for (c,) in con2.execute(
    "SELECT content FROM concepts WHERE retired = 0")]
con2.close()
assert rows and all(r.get("meta_version") == 2 for r in rows), \
    "fallback analyze did not upgrade the store to v2"
print("OK")
"""


def test_ensure_graph_falls_back_to_analyze_on_v1_store(tmp_path):
    """A v1 (or otherwise unloadable) store must not wedge the CLI:
    _ensure_graph falls back to a full analyze, which re-persists the
    store as v2 - the upgrade path."""
    proj = _copy_fixture(tmp_path / "proj")
    # First index it in a subprocess so this test process's global graph
    # starts clean.
    _run_leg("A", proj, tmp_path / "init.json")
    proc = subprocess.run(
        [sys.executable, "-c", FALLBACK_SCRIPT, str(SRC), str(proj)],
        capture_output=True, text=True, timeout=600)
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "OK" in proc.stdout
    # The store is now v2: a fresh cold start succeeds.
    leg_c = _run_leg("C", proj, tmp_path / "c.json",
                     db=_default_db(proj))
    assert leg_c["counts"]["module"] == 7


REUSE_SCRIPT = r"""
import sys
sys.path.insert(0, sys.argv[1])
from pathlib import Path
import coderadar
proj = Path(sys.argv[2])

calls = {"analyze": 0, "load": 0}
_orig_analyze, _orig_load = coderadar.analyze, coderadar.load
def _analyze_spy(*a, **k):
    calls["analyze"] += 1
    return _orig_analyze(*a, **k)
def _load_spy(*a, **k):
    calls["load"] += 1
    return _orig_load(*a, **k)
coderadar.analyze, coderadar.load = _analyze_spy, _load_spy

# Populate the process global with a graph indexed from `proj`.
coderadar.analyze(str(proj), create_store=True)
calls["analyze"] = 0  # the setup call does not count

from coderadar.cli import _ensure_graph
from coderadar._core import graph_stats
_ensure_graph(str(proj))
stats = graph_stats()
assert stats["modules"] == 7, stats
assert calls["analyze"] == 0, "matching loaded graph was not reused"
assert calls["load"] == 0, "loaded instead of reusing the matching graph"
print("REUSED")
"""


def test_ensure_graph_reuses_a_matching_loaded_graph(tmp_path):
    """A graph already indexed from this directory is reused as-is: no
    re-analyze, no cold load."""
    proj = _copy_fixture(tmp_path / "proj")
    proc = subprocess.run(
        [sys.executable, "-c", REUSE_SCRIPT, str(SRC), str(proj)],
        capture_output=True, text=True, timeout=600)
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "REUSED" in proc.stdout


# ── CLI surface ──────────────────────────────────────────────────────────
def test_cli_load_snapshot_command(tmp_path):
    from click.testing import CliRunner
    from coderadar.cli import main as cli_main

    proj = _copy_fixture(tmp_path / "proj")
    _run_leg("A", proj, tmp_path / "init.json")  # index in a subprocess
    res = CliRunner().invoke(
        cli_main, ["load-snapshot", str(_default_db(proj)), "--root",
                   str(proj)])
    assert res.exit_code == 0, res.output


def test_cli_export_command_is_gone(tmp_path):
    from click.testing import CliRunner
    from coderadar.cli import main as cli_main

    res = CliRunner().invoke(cli_main, ["export", str(tmp_path)])
    assert res.exit_code == 2  # click: usage error, no such command
    assert "No such command" in res.output


# ── Staleness heuristic ──────────────────────────────────────────────────
def _make_store_project(tmp_path: Path, db_age_s: float = 60.0,
                        file_ages=()):
    # No default source file: every test states exactly which files
    # exist (and how old they are), so the mtime heuristic is tested
    # against a known set.
    proj = tmp_path / "proj"
    proj.mkdir()
    db = proj / ".coderadar" / "store" / "coderadar.db"
    db.parent.mkdir(parents=True)
    db.write_bytes(b"sqlite-placeholder")
    import time
    now = time.time()
    os.utime(db, (now - db_age_s, now - db_age_s))
    for rel, age in file_ages:
        p = proj / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("X = 1\n")
        os.utime(p, (now - age, now - age))
    return proj, db


def test_store_is_fresh_when_db_is_newest(tmp_path):
    proj, db = _make_store_project(tmp_path, db_age_s=10.0,
                                   file_ages=[("pkg/mod.py", 120.0)])
    assert _cli._store_is_fresh(proj, db) is True


def test_store_is_stale_when_source_is_newer_than_grace(tmp_path):
    proj, db = _make_store_project(tmp_path, db_age_s=10.0,
                                   file_ages=[("pkg/mod.py", 3.0)])
    # source is 3s newer than the db, grace is 2s -> stale
    assert _cli._store_is_fresh(proj, db) is False


def test_store_is_fresh_within_grace(tmp_path):
    proj, db = _make_store_project(tmp_path, db_age_s=10.0,
                                   file_ages=[("pkg/mod.py", 8.5)])
    # source is 1.5s newer than the db, inside the 2s grace -> fresh
    assert _cli._store_is_fresh(proj, db) is True


def test_store_is_stale_when_db_missing(tmp_path):
    proj = tmp_path / "proj"
    proj.mkdir()
    assert _cli._store_is_fresh(proj, proj / ".coderadar" / "store" /
                                "coderadar.db") is False


def test_non_indexable_files_do_not_make_the_store_stale(tmp_path):
    proj, db = _make_store_project(tmp_path, db_age_s=10.0,
                                   file_ages=[("README.md", 1.0),
                                              ("notes.txt", 1.0)])
    assert _cli._store_is_fresh(proj, db) is True


def test_dockerfile_without_extension_counts_as_indexable(tmp_path):
    proj, db = _make_store_project(tmp_path, db_age_s=10.0,
                                   file_ages=[("dockerfile", 1.0)])
    assert _cli._store_is_fresh(proj, db) is False


# ── Store path resolution ────────────────────────────────────────────────
def test_store_db_path_default(tmp_path):
    assert _cli._store_db_path(tmp_path) == tmp_path / ".coderadar" / "store" / "coderadar.db"


def test_store_db_path_relative_config(tmp_path):
    (tmp_path / ".coderadar.toml").write_text(
        '[database]\npath = "store/elsewhere.db"\n', encoding="utf-8")
    assert _cli._store_db_path(tmp_path) == tmp_path / "store" / "elsewhere.db"


def test_store_db_path_absolute_config(tmp_path):
    other = tmp_path / "abs.db"
    # TOML basic string: backslashes must be escaped; repr produces that.
    (tmp_path / ".coderadar.toml").write_text(
        "[database]\npath = %r\n" % str(other), encoding="utf-8")
    assert _cli._store_db_path(tmp_path) == other
