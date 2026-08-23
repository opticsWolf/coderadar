"""The server must not outlive the client.

`serve()` was `create_server(graph)` then `server.run(transport="stdio")`,
with no hygiene at all — so a client that never sent `initialize`, and a
client that was killed rather than closed, both left a server running
forever with an index of a project nobody was working on.
"""

from __future__ import annotations

import os
import threading
import time

import pytest

from coderadar.mcp import lifecycle
from coderadar.mcp.lifecycle import (
    HandshakeTimeout,
    ParentWatchdog,
    make_middleware,
)


@pytest.fixture(autouse=True)
def _no_leftover_handle():
    lifecycle.configure(None)
    yield
    lifecycle.configure(None)


class TestHandshakeTimeout:
    def test_a_silent_client_is_given_up_on(self):
        fired = threading.Event()
        reasons: list[str] = []

        timeout = HandshakeTimeout(
            seconds=0.05, on_timeout=lambda r: (reasons.append(r), fired.set()))
        timeout.arm()

        assert fired.wait(2), "the timeout never fired"
        assert "handshake" in reasons[0]

    def test_a_client_that_speaks_is_kept(self):
        fired = threading.Event()
        timeout = HandshakeTimeout(seconds=0.2, on_timeout=lambda r: fired.set())
        timeout.arm()
        timeout.disarm()

        assert not fired.wait(0.5)
        assert timeout.disarmed

    def test_disarming_twice_is_harmless(self):
        timeout = HandshakeTimeout(seconds=5, on_timeout=lambda r: None)
        timeout.arm()
        timeout.disarm()
        timeout.disarm()
        assert timeout.disarmed

    def test_arming_after_disarm_does_not_resurrect_it(self):
        # The middleware disarms on the first message; nothing should be able
        # to re-arm a timer against a connection that is demonstrably live.
        fired = threading.Event()
        timeout = HandshakeTimeout(seconds=0.05, on_timeout=lambda r: fired.set())
        timeout.disarm()
        timeout.arm()
        assert not fired.wait(0.3)

    def test_a_zero_timeout_disables_the_guard(self):
        fired = threading.Event()
        timeout = HandshakeTimeout(seconds=0, on_timeout=lambda r: fired.set())
        timeout.arm()
        assert not fired.wait(0.2)


class _Ctx:
    def __init__(self, method):
        self.method = method


class TestTheMiddleware:
    def _call(self, method):
        middleware = make_middleware()
        ran = []

        async def call_next(ctx):
            ran.append(ctx)
            return "handler ran"

        import asyncio
        result = asyncio.run(middleware(_Ctx(method), call_next))
        return result, ran

    def test_any_inbound_message_disarms_the_timeout(self):
        # Not just `initialize`: a client that is talking is a client that is
        # there, and refusing to disarm on a message the SDK will reject
        # anyway would kill a live connection.
        timeout = HandshakeTimeout(seconds=5, on_timeout=lambda r: None)
        timeout.arm()
        lifecycle.configure(timeout)

        self._call("tools/list")
        assert timeout.disarmed

    def test_the_handler_still_runs(self):
        result, ran = self._call("initialize")
        assert result == "handler ran"
        assert len(ran) == 1


class TestParentWatchdog:
    def test_a_dead_parent_ends_the_process(self):
        gone = threading.Event()
        reasons: list[str] = []

        watchdog = ParentWatchdog(
            interval=0.02,
            on_orphan=lambda r: (reasons.append(r), gone.set()),
            alive=lambda ppid: False,
        )
        watchdog.start()
        try:
            assert gone.wait(2), "the watchdog never noticed"
            assert str(os.getppid()) in reasons[0]
        finally:
            watchdog.stop()

    def test_a_live_parent_is_left_alone(self):
        gone = threading.Event()
        watchdog = ParentWatchdog(
            interval=0.02, on_orphan=lambda r: gone.set(), alive=lambda ppid: True)
        watchdog.start()
        try:
            assert not gone.wait(0.3)
        finally:
            watchdog.stop()

    def test_stopping_it_stops_the_polling(self):
        polls: list[int] = []
        watchdog = ParentWatchdog(
            interval=0.02, on_orphan=lambda r: None,
            alive=lambda ppid: (polls.append(ppid), True)[1])
        watchdog.start()
        time.sleep(0.1)
        watchdog.stop()
        seen = len(polls)
        time.sleep(0.1)
        assert len(polls) == seen

    def test_our_own_parent_is_alive_right_now(self):
        # The real probe, not the injected one: on Windows the ppid names a
        # process that may no longer exist, so this goes through OpenProcess
        # rather than os.kill — which on Windows would terminate the target.
        watchdog = ParentWatchdog(interval=60, on_orphan=lambda r: None)
        assert watchdog.check_once()

    def test_a_process_that_cannot_exist_reads_as_dead(self):
        watchdog = ParentWatchdog(interval=60, on_orphan=lambda r: None)
        # 0xFFFFFFF is far above any plausible live pid on either platform.
        assert not watchdog._alive(0xFFFFFFF)
