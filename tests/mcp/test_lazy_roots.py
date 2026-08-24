"""The client is asked where the project is — but only on the first tool call.

`roots/list` is a server-to-client request, and awaiting one during
`initialize` deadlocks: the dispatcher reads no further inbound messages
until the handshake returns. So the top rung of the path ladder is climbed
lazily, from middleware, once per connection, and only when startup's answer
was a guess.
"""

from __future__ import annotations

import asyncio
import os
from pathlib import Path

import pytest

from coderadar.mcp import lazy
from coderadar.mcp.lazy import LazyRootRetry, make_middleware
from coderadar.mcp.roots import ResolvedRoot, client_roots
from coderadar.mcp.startup import BackgroundIndex


@pytest.fixture(autouse=True)
def _no_leftover_handles():
    lazy.configure(None)
    yield
    lazy.configure(None)


@pytest.fixture
def here():
    """Restore the cwd — the retry chdirs on purpose."""
    previous = Path(os.getcwd())
    yield previous
    os.chdir(previous)


class _Root:
    def __init__(self, uri):
        self.uri = uri


class _Result:
    def __init__(self, roots):
        self.roots = roots


class _Session:
    """A client that answers roots/list with whatever it was given."""

    def __init__(self, roots, capable=True):
        self._roots = roots
        self._capable = capable
        self.asked = 0

    def check_client_capability(self, capability):
        return self._capable

    async def list_roots(self):
        self.asked += 1
        return _Result([_Root(u) for u in self._roots])


def _project(root: Path) -> Path:
    (root / ".coderadar" / "store").mkdir(parents=True)
    (root / "m.py").write_text("def f(): pass\n", encoding="utf-8")
    return root


def _idle_index() -> BackgroundIndex:
    return BackgroundIndex(analyze=lambda root: None)


class TestWhenWeAsk:
    def test_an_unconfirmed_root_is_worth_asking_about(self, tmp_path):
        resolved = ResolvedRoot(path=tmp_path, source="cwd", marker=None)
        retry = LazyRootRetry(resolved, _idle_index(), index_is_empty=lambda: False)
        assert retry.should_ask()

    def test_a_confirmed_root_with_code_in_it_is_not(self, tmp_path):
        resolved = ResolvedRoot(
            path=tmp_path, source="cwd", marker=tmp_path / ".coderadar")
        retry = LazyRootRetry(resolved, _idle_index(), index_is_empty=lambda: False)
        assert not retry.should_ask()

    def test_a_confirmed_root_that_indexed_nothing_is_worth_asking_about(self, tmp_path):
        # A real directory with no code in it usually means the wrong
        # directory, not a project with no code.
        resolved = ResolvedRoot(
            path=tmp_path, source="cwd", marker=tmp_path / ".coderadar")
        retry = LazyRootRetry(resolved, _idle_index(), index_is_empty=lambda: True)
        assert retry.should_ask()

    def test_we_only_ask_once(self, tmp_path, here):
        resolved = ResolvedRoot(path=tmp_path, source="cwd", marker=None)
        retry = LazyRootRetry(resolved, _idle_index(), index_is_empty=lambda: True)
        asyncio.run(retry.attempt(_Session([])))
        assert not retry.should_ask()


class TestWhatWeDoWithTheAnswer:
    def test_a_better_root_is_adopted_and_reindexed(self, tmp_path, here):
        wrong = tmp_path / "wrong"
        wrong.mkdir()
        right = _project(tmp_path / "right")

        indexed: list[str] = []
        index = BackgroundIndex(analyze=lambda root: indexed.append(os.getcwd()))
        retry = LazyRootRetry(
            ResolvedRoot(path=wrong, source="cwd", marker=None),
            index,
            index_is_empty=lambda: True,
        )

        changed = asyncio.run(retry.attempt(_Session([right.as_uri()])))

        assert changed
        assert retry.resolved.path == right.resolve()
        assert retry.resolved.confirmed
        index.wait(timeout=5)
        assert indexed and Path(indexed[-1]).resolve() == right.resolve(), (
            "the reindex ran from the old directory")

    def test_a_client_that_offers_nothing_changes_nothing(self, tmp_path, here):
        resolved = ResolvedRoot(path=tmp_path, source="cwd", marker=None)
        retry = LazyRootRetry(resolved, _idle_index(), index_is_empty=lambda: True)

        assert not asyncio.run(retry.attempt(_Session([])))
        assert retry.resolved is resolved

    def test_a_client_without_the_roots_capability_is_not_asked(self, tmp_path, here):
        session = _Session([str(tmp_path)], capable=False)
        retry = LazyRootRetry(
            ResolvedRoot(path=tmp_path, source="cwd", marker=None),
            _idle_index(),
            index_is_empty=lambda: True,
        )
        assert not asyncio.run(retry.attempt(session))
        assert session.asked == 0

    def test_we_do_not_trade_a_marker_for_a_guess(self, tmp_path, here):
        """A client's declared workspace is not better evidence than a marker.

        Clients declare where *they* are; a `.coderadar` on disk says where
        the project is. Downgrading confirmed to unconfirmed would make the
        wrong-project failure reachable again from the other direction.
        """
        confirmed = _project(tmp_path / "confirmed")
        bare = tmp_path / "bare"
        bare.mkdir()

        retry = LazyRootRetry(
            ResolvedRoot(
                path=confirmed, source="--path",
                marker=confirmed / ".coderadar"),
            _idle_index(),
            index_is_empty=lambda: True,
        )
        assert not asyncio.run(retry.attempt(_Session([bare.as_uri()])))
        assert retry.resolved.path == confirmed


class TestExplicitChoice:
    """A codegraph_set_project call outranks everything, forever.

    The lazy retry exists because startup's guess can be wrong. Once the
    agent has named the project itself there is nothing left to ask about —
    and asking anyway would let the host's declared workspace override an
    explicit decision, which is exactly backwards.
    """

    def test_marking_suppresses_the_question(self, tmp_path):
        resolved = ResolvedRoot(path=tmp_path, source="cwd", marker=None)
        retry = LazyRootRetry(resolved, _idle_index(), index_is_empty=lambda: True)
        assert retry.should_ask()

        chosen = tmp_path / "chosen"
        chosen.mkdir()
        retry.mark_user_chosen(ResolvedRoot(
            path=chosen, source="project_path", marker=None))
        assert not retry.should_ask()

    def test_the_session_is_never_consulted_after_marking(self, tmp_path):
        session = _Session([])
        resolved = ResolvedRoot(path=tmp_path, source="cwd", marker=None)
        retry = LazyRootRetry(resolved, _idle_index(), index_is_empty=lambda: True)
        retry.mark_user_chosen(ResolvedRoot(
            path=tmp_path / "x", source="project_path", marker=None))
        asyncio.run(retry.attempt(session))
        assert session.asked == 0

    def test_the_recorded_root_is_described_afterwards(self, tmp_path):
        chosen = _project(tmp_path / "chosen")
        retry = LazyRootRetry(
            ResolvedRoot(path=tmp_path, source="cwd", marker=None),
            _idle_index(),
        )
        retry.mark_user_chosen(ResolvedRoot(
            path=chosen,
            source="project_path",
            marker=chosen / ".coderadar",
        ))
        assert "chosen" in retry.describe()
        assert "marker .coderadar" in retry.describe()


class _BrokenSession:
    def check_client_capability(self, capability):
        return True

    async def list_roots(self):
        raise RuntimeError("client went away")


class TestClientRoots:
    def test_a_session_that_raises_yields_no_roots(self):
        # A client that will not answer is a reason to keep the root we have,
        # not to fail the tool call that triggered the question.
        assert asyncio.run(client_roots(_BrokenSession())) == []

    def test_uris_are_returned_verbatim(self, tmp_path):
        uri = tmp_path.as_uri()
        assert asyncio.run(client_roots(_Session([uri]))) == [uri]


class _Ctx:
    def __init__(self, method, session):
        self.method = method
        self.session = session


class TestTheMiddleware:
    def _run(self, method, retry=None):
        lazy.configure(retry)
        middleware = make_middleware()
        seen = []
        ctx = _Ctx(method, _Session([]))

        async def call_next(c):
            seen.append(c)
            return "handler ran"

        result = asyncio.run(middleware(ctx, call_next))
        return result, seen, ctx

    def test_the_handler_always_runs(self, tmp_path):
        result, seen, _ = self._run("tools/call")
        assert result == "handler ran"
        assert len(seen) == 1

    def test_only_tool_calls_trigger_the_question(self, tmp_path, here):
        retry = LazyRootRetry(
            ResolvedRoot(path=tmp_path, source="cwd", marker=None),
            _idle_index(),
            index_is_empty=lambda: True,
        )
        _, _, ctx = self._run("tools/list", retry)
        assert ctx.session.asked == 0

        _, _, ctx = self._run("tools/call", retry)
        assert ctx.session.asked == 1

    def test_a_failing_retry_does_not_fail_the_tool_call(self, tmp_path):
        retry = _Exploding(
            ResolvedRoot(path=tmp_path, source="cwd", marker=None), _idle_index())
        result, _, _ = self._run("tools/call", retry)
        assert result == "handler ran"


class _Exploding(LazyRootRetry):
    def should_ask(self):
        return True

    async def attempt(self, session):
        raise RuntimeError("boom")


class TestGuidance:
    def test_the_no_index_message_names_the_directory(self, tmp_path):
        from coderadar.mcp import server as server_mod

        lazy.configure(LazyRootRetry(
            ResolvedRoot(path=tmp_path, source="cwd", marker=None), _idle_index()))
        message = server_mod._no_index_message()

        assert str(tmp_path) in message
        assert "cwd" in message
        # An agent served the wrong project needs to be told how to say so.
        assert "--path" in message

    def test_a_confirmed_root_is_told_to_reindex_not_to_move(self, tmp_path):
        from coderadar.mcp import server as server_mod

        lazy.configure(LazyRootRetry(
            ResolvedRoot(
                path=tmp_path, source="--path", marker=tmp_path / ".coderadar"),
            _idle_index()))
        message = server_mod._no_index_message()

        assert "codegraph_reindex" in message
        # A confirmed root is not something to send the user off to change.
        assert "restart the server" not in message
