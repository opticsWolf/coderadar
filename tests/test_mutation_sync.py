"""Mutation synchronization outcomes across the FFI/Python boundary.

These tests pin the contract in `docs/mutation-rollback-design.md` §4:
the Rust engine reports an internal `disk_status`, and the Python facade
assigns a public status only after it proves the synchronized bytes. The
fake `update_file` reports here stand in for an external writer winning
the race between a verified disk write and graph synchronization.
"""

from __future__ import annotations

import json
import threading

import pytest

try:
    from coderadar import (
        CodeGraph,
        MutationResult,
        MutationSyncSummary,
        UpdateReport,
        _core,
        render,
    )
    from coderadar._core import analyze as _analyze
    from coderadar._core import apply_mutation as _apply_mutation
    from coderadar.cli import _mutation_exit_code, _mutation_failure_exit_code
    from coderadar.mutation.tool_router import ToolRouter
    _CORE = True
except ImportError:  # pragma: no cover
    _CORE = False

pytestmark = pytest.mark.skipif(not _CORE, reason="Rust _core extension not built")


SOURCE = """\
def greet(name):
    return "hello " + name
"""


@pytest.fixture
def project(tmp_path, monkeypatch):
    (tmp_path / "app.py").write_text(SOURCE, encoding="utf-8")
    monkeypatch.chdir(tmp_path)
    _analyze(".")
    yield tmp_path


def _entity_id(name: str) -> str:
    from coderadar._core import search_entities

    for hit in search_entities(name, 50):
        if hit.get("name") == name:
            return hit["id"]
    raise AssertionError(f"{name} is not in the index")


def _report(target: str, source_hash: str) -> UpdateReport:
    return UpdateReport(
        affected_files=[target],
        changed_symbols=[],
        new_unresolved_references=[],
        newly_resolved_references=[],
        elapsed_ms=0.0,
        parse_quality="Clean",
        parse_errors=0,
        fully_applied=True,
        epoch_before=0,
        epoch_after=1,
        source_hash=source_hash,
    )


def test_apply_mutation_reports_internal_disk_status_not_public_success(project):
    plan = CodeGraph().plan_replace_body(_entity_id("greet"), 'return "hi"', dry_run=True)
    edit = plan.edits[0]
    raw = _apply_mutation(json.dumps({
        "id": plan.id,
        "tool": plan.tool,
        "edits": [{
            "file": edit.file,
            "span_start": edit.span_start or 0,
            "span_end": edit.span_end or 0,
            "replacement": edit.replacement,
            "expected_hash": edit.expected_hash or "",
        }],
        "affected_files": plan.affected_files,
    }))

    assert raw["disk_status"] == "Installed"
    assert "applied" not in raw
    assert "status" not in raw
    assert raw["attempt_id"]
    assert raw["primary"] is None
    assert raw["failure"] is None
    assert raw["files_written"] == [edit.file]
    assert raw["errors"] == []
    assert raw["backup_path"] is None
    assert raw["sync"]["state"] == "NotAttempted"


def test_rejected_plan_needs_no_synchronization(project):
    from coderadar import MutationEdit, MutationPlan

    outsider = project.parent / "outside.py"
    outsider.write_text("secret = 1\n", encoding="utf-8")
    try:
        plan = MutationPlan(
            id="handcrafted",
            tool="create_entity",
            edits=[MutationEdit(
                file=str(outsider),
                replacement="owned = 1\n",
                span_start=0,
                span_end=0,
            )],
            affected_files=[str(outsider)],
            diff_preview="",
            unverified_sites=[],
            warnings=[],
        )
        result = CodeGraph().apply(plan)
    finally:
        outsider.unlink()

    assert result.status == "RejectedPolicy"
    assert result.files_written == []
    assert result.backup_path is None
    assert result.sync.state == "NotNeeded"
    assert result.sync.attempted_files == []


def test_synchronization_covers_affected_files_without_expected_hashes(project, monkeypatch):
    from coderadar import MutationPlan

    updated: list[str] = []

    def _updated(self, file_path, content=None, force=False):
        updated.append(file_path)
        return _report(file_path, "EXPECTED" if file_path == "app.py" else "OTHER")

    def _cleared(file_path):
        return {"ok": True, "cleared": 0}

    raw = {
        "disk_status": "Installed",
        "attempt_id": "union",
        "primary": None,
        "failure": None,
        "files": [
            {
                "file": "app.py",
                "state": "Installed",
                "pre_hash": "OLD",
                "installed_hash": "EXPECTED",
                "final_hash": "EXPECTED",
                "expected_hash": "EXPECTED",
                "recovery_path": None,
                "error": None,
            },
            {
                "file": "other.py",
                "state": "Untouched",
                "pre_hash": "",
                "installed_hash": None,
                "final_hash": None,
                "expected_hash": None,
                "recovery_path": None,
                "error": None,
            },
        ],
        "files_written": ["app.py"],
        "errors": [],
        "backup_path": None,
        "sync": {
            "state": "NotAttempted",
            "attempted_files": [],
            "updated_files": [],
            "errors": [],
            "warnings": [],
        },
    }
    monkeypatch.setattr(_core, "apply_mutation", lambda payload: raw)
    monkeypatch.setattr(CodeGraph, "update_file", _updated)
    monkeypatch.setattr(_core, "clear_embeddings_for_file", _cleared)
    plan = MutationPlan(
        id="union",
        tool="replace_entity_body",
        edits=[],
        affected_files=["app.py", "other.py"],
        diff_preview="",
        unverified_sites=[],
        warnings=[],
    )
    result = CodeGraph().apply(plan)

    assert result.status == "Applied"
    assert result.sync.state == "Synchronized"
    assert result.sync.attempted_files == ["app.py", "other.py"]
    assert set(updated) == {"app.py", "other.py"}


def test_apply_incomplete_when_sync_sees_another_writers_bytes(project, monkeypatch):
    target = project / "app.py"
    graph = CodeGraph()
    plan = graph.plan_replace_body(_entity_id("greet"), 'return "hi"', dry_run=True)

    def _wrong_bytes(self, file_path, content=None, force=False):
        assert file_path == "app.py"
        return _report(file_path, "0" * 16)

    monkeypatch.setattr(CodeGraph, "update_file", _wrong_bytes)
    result = graph.apply(plan)

    assert result.status == "ApplyIncomplete"
    assert '"hi"' in target.read_text(encoding="utf-8")
    assert result.files_written == ["app.py"]
    assert result.sync.state == "Failed"
    assert result.failure is not None
    assert (result.failure.stage, result.failure.code) == ("graph_sync", "concurrent_modification")
    out = render.mutation_applied(result)
    assert "## Mutation Incomplete — Graph Sync Failed" in out
    assert "**Graph updated:** no — synchronization did not complete" in out
    assert "**Sync:** Failed" in out


def test_rollback_incomplete_when_restored_sync_sees_another_writers_bytes(
    project, monkeypatch
):
    from coderadar import MutationEdit, MutationPlan

    target = project / "app.py"
    graph = CodeGraph()
    plan = graph.plan_replace_body(_entity_id("greet"), "return 2", dry_run=True)
    edit = plan.edits[0]
    broken = MutationPlan(
        id="t",
        tool="replace_entity_body",
        edits=[MutationEdit(
            file=edit.file,
            replacement="return (",
            expected_hash=edit.expected_hash,
            span_start=edit.span_start,
            span_end=edit.span_end,
        )],
        affected_files=[edit.file],
        diff_preview="",
        unverified_sites=[],
        warnings=[],
    )

    def _wrong_bytes(self, file_path, content=None, force=False):
        assert file_path == "app.py"
        return _report(file_path, "0" * 16)

    monkeypatch.setattr(CodeGraph, "update_file", _wrong_bytes)
    result = graph.apply(broken)

    assert result.status == "RollbackIncomplete"
    assert target.read_text(encoding="utf-8") == SOURCE
    assert result.files_written == []
    assert result.files[0].state == "Restored"
    assert result.primary is not None
    assert result.primary.code == "introduced_syntax_error"
    assert result.failure is not None
    assert (result.failure.stage, result.failure.code) == ("graph_sync", "concurrent_modification")
    assert result.sync.state == "Failed"
    assert result.backup_path is None
    out = render.mutation_applied(result)
    assert "## Mutation Incomplete — Rollback Incomplete" in out
    assert "**Graph updated:** no" in out


def test_tool_router_propagates_structured_mutation_outcomes():
    failed = MutationResult(
        status="ApplyIncomplete",
        files_written=["app.py"],
        syntax_errors=[],
        sync=MutationSyncSummary(
            state="Failed",
            attempted_files=["app.py"],
            updated_files=[],
            errors=[{
                "stage": "graph_sync",
                "code": "concurrent_modification",
                "file": "app.py",
                "message": "another writer won the race",
            }],
            warnings=[],
        ),
    )
    payload = ToolRouter._mutation_payload(failed)
    error = ToolRouter._mutation_error(failed)

    assert payload["status"] == "ApplyIncomplete"
    assert payload["files_written"] == ["app.py"]
    assert payload["sync"]["state"] == "Failed"
    assert "graph_sync/concurrent_modification" in (error or "")

    assert ToolRouter._mutation_error(MutationResult(
        status="Applied", files_written=["app.py"], syntax_errors=[]
    )) is None


def test_concurrent_apply_calls_share_one_operation_lock(project, monkeypatch):
    import coderadar
    from coderadar import MutationPlan

    real_lock = coderadar._mutation_operation_lock()
    attempts = 0
    attempts_guard = threading.Lock()
    entered = threading.Event()
    second_attempted = threading.Event()
    proceed = threading.Event()
    fake_entries: list[int] = []

    class _LockGate:
        def __enter__(self):
            nonlocal attempts
            with attempts_guard:
                attempts += 1
                if attempts == 2:
                    second_attempted.set()
            real_lock.acquire()
            return self

        def __exit__(self, *exc_info):
            real_lock.release()
            return False

    monkeypatch.setattr(coderadar, "_mutation_operation_lock", lambda: _LockGate())
    raw = {
        "disk_status": "Installed",
        "attempt_id": "serialized",
        "primary": None,
        "failure": None,
        "files": [{
            "file": "app.py",
            "state": "Installed",
            "pre_hash": "OLD",
            "installed_hash": "EXPECTED",
            "final_hash": "EXPECTED",
            "expected_hash": "EXPECTED",
            "recovery_path": None,
            "error": None,
        }],
        "files_written": ["app.py"],
        "errors": [],
        "backup_path": None,
        "sync": {
            "state": "NotAttempted",
            "attempted_files": [],
            "updated_files": [],
            "errors": [],
            "warnings": [],
        },
    }

    def _fake_apply(payload):
        fake_entries.append(threading.get_ident())
        if len(fake_entries) == 1:
            entered.set()
            assert proceed.wait(timeout=10)
        return raw

    def _updated(self, file_path, content=None, force=False):
        return _report(file_path, "EXPECTED")

    monkeypatch.setattr(_core, "apply_mutation", _fake_apply)
    monkeypatch.setattr(CodeGraph, "update_file", _updated)
    monkeypatch.setattr(
        _core, "clear_embeddings_for_file", lambda file_path: {"ok": True, "cleared": 0}
    )
    plan = MutationPlan(
        id="serialized",
        tool="replace_entity_body",
        edits=[],
        affected_files=["app.py"],
        diff_preview="",
        unverified_sites=[],
        warnings=[],
    )
    outcomes: dict[str, MutationResult] = {}
    errors: dict[str, BaseException] = {}

    def _worker(name):
        try:
            outcomes[name] = CodeGraph().apply(plan)
        except Exception as exc:  # noqa: BLE001 - join must surface worker failures
            errors[name] = exc

    first = threading.Thread(target=_worker, args=("first",))
    first.start()
    assert entered.wait(timeout=10)
    second = threading.Thread(target=_worker, args=("second",))
    second.start()
    assert second_attempted.wait(timeout=10)
    assert len(fake_entries) == 1
    proceed.set()
    first.join(timeout=10)
    second.join(timeout=10)
    assert errors == {}
    assert fake_entries == [first.ident, second.ident]
    assert outcomes["first"].status == "Applied"
    assert outcomes["second"].status == "Applied"


def test_mutation_failure_exit_codes_match_plan_time_refusals():
    from coderadar.ops import MutationFailed

    assert _mutation_failure_exit_code(MutationFailed(
        "PolicyViolation { path: 'x.py', reason: 'policy rejected the plan' }"
    )) == 2
    assert _mutation_failure_exit_code(MutationFailed(
        "StaleIndex { file: 'x.py', expected: 'a', span: 'b' }"
    )) == 2
    assert _mutation_failure_exit_code(MutationFailed("EntityNotFound('x')")) == 1


def test_mutation_exit_codes_match_the_public_contract():
    assert _mutation_exit_code({"plan": None, "result": None}) == 0
    assert _mutation_exit_code({"plan": None, "result": {"status": "Applied"}}) == 0
    assert _mutation_exit_code({"plan": None, "result": {"status": "RolledBack"}}) == 1
    assert _mutation_exit_code({"plan": None, "result": {"status": "ApplyIncomplete"}}) == 1
    assert _mutation_exit_code({"plan": None, "result": {"status": "RollbackIncomplete"}}) == 1
    assert _mutation_exit_code({"plan": None, "result": {"status": "RejectedStale"}}) == 2
    assert _mutation_exit_code({"plan": None, "result": {"status": "RejectedPolicy"}}) == 2
