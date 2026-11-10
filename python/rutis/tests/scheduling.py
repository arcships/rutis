"""Controlling the event loop in tests (#630, formal verification design
§3.3: "Python: the event loop, cancel propagation, deferred callbacks and
frames during close").

Python has no scheduler seam like Node's pump: a synchronous call blocks the
loop thread in `Condition.wait` (peer.py `_call_sync`), so a test cannot step
the loop while a call waits. What can be controlled is controlled here:

* `SteppedLoop`: a `SelectorEventLoop` whose ready callbacks run only when the
  test says so, in the loop's own FIFO order, and whose clock moves only when
  the test advances it. Tests never call `run_forever`; a timer is measured in
  test steps, not seconds.
* `ScriptedChannel`: the channel a session reads from, where the test hands
  over one incoming frame at a time and takes outgoing frames in order, so the
  next frame is handed over only after the previous one was answered.

`SteppedLoop.entered()` marks the loop as the running one for the duration of
the block: asyncio objects such as `Event` ask `get_running_loop()` when they
are used, and a loop that is stepped rather than run is not "running" on its
own. That is `asyncio.events._set_running_loop`, a CPython internal; the
Python runtime is CPython-only here (the rest of the suite makes the same
assumption through `IsolatedAsyncioTestCase`).
"""

from __future__ import annotations

import asyncio
import collections
import contextlib
import heapq
import json
import threading
import time

from support import HANG_GUARD, Hung


class SteppedLoop(asyncio.SelectorEventLoop):
    """An event loop the test steps: ready callbacks run on `step`, timers
    become ready on `advance`, and the clock is virtual."""

    def __init__(self) -> None:
        super().__init__()
        self._now = 0.0

    def time(self) -> float:
        return self._now

    # ── Running ──────────────────────────────────────────────────

    def step(self, count: int = 1) -> int:
        """Run up to `count` ready callbacks, in the order asyncio queued
        them; the number actually run."""
        ran = 0
        while ran < count and self._ready:
            handle = self._ready.popleft()
            if handle._cancelled:
                continue
            handle._run()
            ran += 1
        return ran

    def run_ready(self) -> int:
        """Run every callback that is ready now. A timer that is not due yet
        is not ready and does not run."""
        return self.step(len(self._ready))

    def run_until_idle(self, what: str = "the loop to go idle") -> None:
        """`run_ready` until nothing is ready and no timer is left; a wait
        outlasting HANG_GUARD fails as hung."""
        deadline = time.monotonic() + HANG_GUARD
        while self._ready or self._scheduled:
            self.run_ready()
            if not self._ready and self._scheduled:
                # Only timers are left; nothing to do without advancing time.
                return
            if time.monotonic() > deadline:
                raise Hung(what)

    def advance(self, seconds: float) -> int:
        """Move the clock forward; timers due by then become ready. The
        callbacks themselves run on the next `step`; returns how many became
        ready."""
        self._now += seconds
        ready = 0
        while self._scheduled and self._scheduled[0]._when <= self._now:
            handle = heapq.heappop(self._scheduled)
            handle._scheduled = False
            if not handle._cancelled:
                self._ready.append(handle)
                ready += 1
        return ready

    # ── Lifecycle ────────────────────────────────────────────────

    @contextlib.contextmanager
    def entered(self):
        """Mark this loop as the running one for asyncio objects that ask,
        without running it."""
        previous = asyncio.events._get_running_loop()
        asyncio.events._set_running_loop(self)
        try:
            yield self
        finally:
            asyncio.events._set_running_loop(previous)

    def finish(self) -> None:
        """Cancel what is still pending, let the cancellations run, and close
        the loop: a test that fails midway leaves no task behind."""
        for task in asyncio.all_tasks(self):
            task.cancel()
        for _ in range(32):
            if not self.run_ready():
                break
        self.close()


class ScriptedChannel:
    """A channel whose incoming frames the test hands over one at a time.

    `feed` queues a frame for the session's reader; `take` waits for an
    outgoing frame that matches, so a test hands over the next frame only
    after it saw the answer to the previous one. Every wait is bounded by
    HANG_GUARD, which only guards against a hang.
    """

    max_message = 16 * 1024 * 1024

    def __init__(self) -> None:
        self._cv = threading.Condition()
        self._incoming: collections.deque[bytes] = collections.deque()
        self._sent: list[dict] = []
        self._closed: str | None = None
        self.reads = 0

    def feed(self, frame: dict) -> None:
        """Hand `frame` to the session's reader, as the far end would."""
        message = json.dumps(frame).encode()
        with self._cv:
            self._incoming.append(message)
            self._cv.notify_all()

    def send(self, message: bytes) -> None:
        frame = json.loads(message)
        with self._cv:
            self._sent.append(frame)
            self._cv.notify_all()

    def recv(self) -> bytes | None:
        with self._cv:
            while not self._incoming and self._closed is None:
                self._cv.wait()
            self.reads += 1
            if self._incoming:
                return self._incoming.popleft()
            return None

    def close(self, reason: str = "") -> None:
        with self._cv:
            if self._closed is None:
                self._closed = reason
            self._cv.notify_all()

    def pending(self) -> list[dict]:
        """The frames sent so far that the test has not taken, in order."""
        with self._cv:
            return list(self._sent)

    def take(self, what: str, matches=lambda frame: True) -> dict:
        """The first frame (already sent, or the next one) that `matches`,
        removed from the queue; the far end's tape."""
        deadline = time.monotonic() + HANG_GUARD
        with self._cv:
            while True:
                for index, frame in enumerate(self._sent):
                    if matches(frame):
                        return self._sent.pop(index)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise AssertionError(
                        f"timed out ({HANG_GUARD} s): {what}; sent so far: {self._sent!r}"
                    )
                self._cv.wait(remaining)
