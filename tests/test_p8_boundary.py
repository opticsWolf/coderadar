"""P8: the data boundary — the store's own rows leave and come back.

A-9 splits the store in two: everything but annotations and comments is
derived from git; those two kinds are the store's own data. INV-18 is
the promise tested here: a store rebuilt from the same repository and the
same export answers every read identically.

The rebuild is exercised in a **subprocess** on purpose. It moves the
store file it reads, so it needs a process with no graph loaded; that is
also the CLI's situation, and asserting it here keeps the precondition
honest instead of incidental.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from coderadar import load, ops

try:
    from coderadar import _core
except ImportError:  # pragma: no cover - CI without the extension
    _core = None  # type: ignore[assignment]

pytestmark = pytest.mark.skipif(
    _core is None, reason="Rust _core extension not built")

NL = chr(10)
SRC = str(Path(__file__).parent.parent / "py_agent" / "src")
ENTITY = "a.py::a"
A_BODY = f"def a():{NL}    return 1{NL}"
B_BODY = f"def b():{NL}    return 2{NL}"

# One script, two modes: "write" mints the rows and exports them; "read"
# answers the same reads from whatever store it is pointed at. Both print
# one JSON line, so the parent compares dicts instead of re-deriving them.
PROBE = """\
import json
import sys

sys.path.insert(0, __SRC__)
from coderadar import load, ops

store, repo, export_path, entity, mode = sys.argv[1:6]
NL = chr(10)
if mode == "build":
    # The whole fixture in one short-lived process: the rebuild test needs
    # a parent that has never opened this store (the engine holds a store
    # file for the life of the process that opened it).
    ops.ingest_history(repo, store_path=store)
graph = load(store, repo)
if mode in ("write", "build"):
    first = graph.annotate(entity, "- [ ] open item" + NL + "- [x] closed" + NL)["id"]
    graph.revise_annotation(
        first, "- [ ] open item" + NL + "- [x] closed" + NL + "- [ ] third" + NL)
    graph.annotate(entity, "- second annotation" + NL)
    graph.comment(entity, "a note" + NL)
    retired = graph.annotate(entity, "- retire me" + NL)["id"]
    graph.retire_annotation(retired)
    export = ops.export_annotations(export_path, graph=graph)


def strip(versions):
    keys = ("id", "body", "valid_from", "valid_to", "retired", "current")
    return [{k: v.get(k) for k in keys} for v in versions]


summary = {
    "annotations": strip(graph.annotations_on(entity)["versions"]),
    "comments": strip(graph.comments_on(entity)["versions"]),
    "entities": sorted(r["id"] for r in graph.query("entities")),
    "history": [[v.get("id"), v.get("valid_from")]
                for v in graph.history_of("a.py")["versions"]],
}
if mode in ("write", "build"):
    summary["export"] = export
print(json.dumps(summary, sort_keys=True))
"""

REBUILD = """\
import json
import sys

sys.path.insert(0, __SRC__)
from coderadar import ops

report = ops.rebuild(sys.argv[1], store_path=sys.argv[2])
print(json.dumps(report, sort_keys=True))
"""


def _script(tmp_path: Path, text: str, name: str) -> Path:
    path = tmp_path / name
    path.write_text(text.replace("__SRC__", repr(SRC)), encoding="utf-8",
                    newline="")
    return path


def _run(script: Path, *args, cwd: Path) -> dict:
    done = subprocess.run([sys.executable, str(script), *map(str, args)],
                          cwd=str(cwd), capture_output=True, text=True)
    assert done.returncode == 0, done.stderr
    return json.loads(done.stdout.strip().splitlines()[-1])


def _git(repo, *args, env=None):
    merged = dict(os.environ)
    if env:
        merged.update(env)
    subprocess.run(["git", *args], cwd=str(repo), env=merged,
                   check=True, capture_output=True)


def _commit(repo, message, date, files):
    for name, content in files.items():
        target = repo / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8", newline=NL)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", message, env={
        "GIT_AUTHOR_NAME": "T", "GIT_AUTHOR_EMAIL": "t@t.t",
        "GIT_COMMITTER_NAME": "T", "GIT_COMMITTER_EMAIL": "t@t.t",
        "GIT_AUTHOR_DATE": date, "GIT_COMMITTER_DATE": date,
    })


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main", ".")
    _git(repo, "config", "user.name", "T")
    _git(repo, "config", "user.email", "t@t.t")
    _git(repo, "config", "core.autocrlf", "false")
    _commit(repo, "c1", "2026-01-01T10:00:00+00:00",
            {"a.py": A_BODY, "b.py": B_BODY, ".gitignore": ".coderadar/" + NL})
    _commit(repo, "c2", "2026-01-02T10:00:00+00:00",
            {"a.py": A_BODY + f"{NL}def later():{NL}    return 3{NL}"})
    return repo


def _store(repo: Path) -> Path:
    return repo / ".coderadar" / "store" / "coderadar.db"


@pytest.fixture
def exported(tmp_path):
    """A store with every row shape, and the export written from it.

    Built entirely in a child process: the rebuild test moves the store
    file, which needs a parent that has never opened it.
    """
    repo = _repo(tmp_path)
    store = _store(repo)
    export_path = tmp_path / "annotations.json"
    probe = _script(tmp_path, PROBE, "probe.py")
    before = _run(probe, store, repo, export_path, ENTITY, "build", cwd=repo)
    return repo, store, export_path, probe, before


class TestTheDocument:
    def test_export_carries_every_chain_in_order(self, exported):
        _, _, export_path, _, before = exported
        assert before["export"]["rows"] == 4
        assert before["export"]["versions"] == 5
        document = json.loads(export_path.read_text(encoding="utf-8"))
        assert document["format"] == "coderadar.annotations.v1"
        ids = [r["id"] for r in document["rows"]]
        assert ids == sorted(ids)
        by_id = {r["id"]: r for r in document["rows"]}
        chain = by_id[f"annotation:{ENTITY}:1"]["versions"]
        assert [v["body"] for v in chain] == [
            f"- [ ] open item{NL}- [x] closed{NL}",
            f"- [ ] open item{NL}- [x] closed{NL}- [ ] third{NL}",
        ]
        # The chain's intervals are contiguous and ascend: a superseded
        # version closes exactly where the next one starts.
        assert chain[0]["valid_from"] < chain[1]["valid_from"]
        assert chain[0]["valid_to"] == chain[1]["valid_from"]
        assert chain[1]["valid_to"] == "9999-12-31T23:59:59.999999Z"
        assert [v["retired"] for v in chain] == [False, False]
        retired = by_id[f"annotation:{ENTITY}:3"]["versions"]
        assert len(retired) == 1 and retired[0]["retired"] is True
        assert retired[0]["valid_to"] != "9999-12-31T23:59:59.999999Z"

    def test_a_comment_and_an_annotation_keep_their_kind(self, exported):
        _, _, export_path, _, _ = exported
        document = json.loads(export_path.read_text(encoding="utf-8"))
        kinds = {r["id"]: r["kind"] for r in document["rows"]}
        assert kinds[f"comment:{ENTITY}:1"] == "comment"
        assert kinds[f"annotation:{ENTITY}:2"] == "annotation"


class TestTheImport:
    def test_a_fresh_store_reads_identically(self, exported):
        repo, _, export_path, probe, before = exported
        fresh = repo / ".coderadar" / "store" / "fresh.db"
        ops.ingest_history(str(repo), store_path=str(fresh))

        report = ops.import_annotations(str(export_path), store_path=str(fresh))

        assert report["imported"] == 4 and report["verified"] == 4
        after = _run(probe, fresh, repo, export_path, ENTITY, "read", cwd=repo)
        assert after == {k: v for k, v in before.items() if k != "export"}

    def test_an_existing_id_refuses_and_skip_reports_it(self, exported):
        _, store, export_path, _, _ = exported
        with pytest.raises(ops.InvalidRequest, match="already exist"):
            ops.import_annotations(str(export_path), store_path=str(store))
        report = ops.import_annotations(
            str(export_path), store_path=str(store), on_conflict="skip")
        assert report["imported"] == 0
        assert sorted(report["skipped"]) == [
            f"annotation:{ENTITY}:1", f"annotation:{ENTITY}:2",
            f"annotation:{ENTITY}:4".replace(":4", ":3"),
            f"comment:{ENTITY}:1",
        ]

    def test_validation_runs_before_any_write(self, exported):
        _, store, export_path, _, _ = exported
        document = json.loads(export_path.read_text(encoding="utf-8"))
        bad = json.loads(json.dumps(document))
        bad["rows"][0]["versions"].reverse()
        with pytest.raises(ops.InvalidRequest, match="not after|starts at"):
            ops.import_annotations(bad, store_path=str(store))
        foreign = json.loads(json.dumps(document))
        foreign["format"] = "coderadar.annotations.v2"
        with pytest.raises(ops.InvalidRequest, match="export format"):
            ops.import_annotations(foreign, store_path=str(store))
        lying = json.loads(json.dumps(document))
        lying["rows"][0]["n"] = 99
        with pytest.raises(ops.InvalidRequest, match="disagrees with its own id"):
            ops.import_annotations(lying, store_path=str(store))


class TestTheRebuild:
    def test_inv18_every_read_answers_identically(self, exported, tmp_path):
        repo, store, export_path, probe, before = exported
        rebuild = _script(tmp_path, REBUILD, "rebuild.py")

        report = _run(rebuild, repo, store, cwd=repo)

        assert report["verified"] is True
        assert report["imported"]["imported"] == 4
        assert report["imported"]["versions"] == 5
        assert report["commits_ingested"] == 2, report["branch_reports"]
        assert Path(report["backup"]).exists(), report["backup"]
        assert Path(report["export"]["path"]).exists()
        assert report["embeddings"] == {"embedded": False}

        after = _run(probe, store, repo, export_path, ENTITY, "read", cwd=repo)
        assert after == {k: v for k, v in before.items() if k != "export"}

    def test_a_loaded_graph_refuses_before_the_move(self, exported):
        repo, store, _, _, _ = exported
        load(str(store), str(repo))
        with pytest.raises(ops.InvalidRequest, match="loaded in this process"):
            ops.rebuild(str(repo), store_path=str(store))
        # Nothing moved: the store is still where it was.
        assert store.exists()
