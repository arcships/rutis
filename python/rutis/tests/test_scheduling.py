"""Scheduling the Python session's frames, controlled by the test (#630,
formal verification design §3.3: "Python: the event loop, cancel
propagation, deferred callbacks and frames during close").

Nothing here waits on a wall clock: `SteppedLoop` runs the loop's ready
callbacks only when the test steps it and moves the clock only when the test
advances it; `ScriptedChannel` hands over one incoming frame at a time and
waits for the answer to the previous one. `tests/scheduling.py` says how.
"""

import asyncio
import threading

import background
from scheduling import ScriptedChannel, SteppedLoop
from support import HANG_GUARD, LOCAL, hello

from rutis.peer import Peer


def data(value):
    return {"type": "data", "value": value}


def invoke(call, method, args=None, sync=False):
    frame = {
        "op": "invoke",
        "id": call,
        "path": [],
        "target": "test",
        "method": method,
        "args": args or {"type": "list", "value": []},
    }
    if sync:
        frame["sync"] = True
    return frame


class DeferredFrameTests(background.TestCase):
    """A received frame is a job the loop runs, not work the reader does."""

    def setUp(self):
        self.loop = SteppedLoop()
        self.channel = ScriptedChannel()
        self.log = []

    def tearDown(self):
        self.loop.finish()

    def dispatch(self, target, method, args):
        self.log.append(method)
        return f"answer:{method}"

    def test_frames_run_in_arrival_order_and_only_when_the_loop_runs_them(self):
        with self.loop.entered():
            peer = Peer(self.channel, self.dispatch, endpoint=LOCAL)
            peer.receive(hello())
            peer.receive(invoke("main:1", "first"))
            peer.receive(invoke("main:2", "second"))
            self.assertEqual(self.log, [], "a received frame runs only when the loop runs it")
            self.assertEqual(self.loop.step(), 1)
            self.assertEqual(self.log, ["first"], "one step ran one job")
            self.assertEqual(
                self.channel.pending(),
                [{"op": "return", "id": "main:1", "value": data("answer:first")}],
            )
            self.assertEqual(self.loop.step(), 1)
            self.assertEqual(self.log, ["first", "second"])
            self.assertEqual(
                [frame["id"] for frame in self.channel.pending()],
                ["main:1", "main:2"],
                "the answers keep the frame order",
            )
            peer.close()

    def test_a_timer_runs_on_the_test_s_clock(self):
        # The loop's clock is virtual: the callback a plugin scheduled 30
        # seconds out runs when the test advances 30 seconds, not before.
        with self.loop.entered():
            fired = []

            def fire():
                peer.notify("host", "timer", [])
                fired.append(True)

            def dispatch(target, method, args):
                if method == "arm":
                    self.loop.call_later(30, fire)
                    return "armed"
                return method

            peer = Peer(self.channel, dispatch, endpoint=LOCAL)
            peer.receive(hello())
            peer.receive(invoke("main:1", "arm"))
            self.loop.run_ready()
            self.assertEqual(self.channel.take("the answer", lambda frame: frame["op"] == "return"), {
                "op": "return", "id": "main:1", "value": data("armed"),
            })
            self.assertEqual(fired, [], "30 s have not passed")
            self.assertEqual(self.loop.advance(29), 0, "no timer is due at 29 s")
            self.loop.run_ready()
            self.assertEqual(fired, [])
            self.assertEqual(self.loop.advance(1), 1, "the 30 s timer is due")
            self.loop.run_ready()
            self.assertEqual(fired, [True], "the timer asked the far end, once")
            # The timer ran in the call's context: the notification carries
            # the chain of the call that scheduled it.
            self.assertEqual(
                [frame for frame in self.channel.pending() if frame["op"] == "invoke"],
                [{
                    "op": "invoke", "id": "py:1", "path": ["main:1"], "target": "host", "method": "timer",
                    "args": {"type": "data", "value": []},
                }],
            )
            peer.close()


class CancelTests(background.TestCase):
    """Cancellation travels through the loop: a call's task is found and
    cancelled when the cancel is read, and its signal is set."""

    def setUp(self):
        self.loop = SteppedLoop()
        self.channel = ScriptedChannel()
        self.events = []

    def tearDown(self):
        self.loop.finish()

    # guarantee: bridge/cancel-reaches-callee
    def test_a_cancel_read_while_the_method_runs_reaches_its_signal_and_its_task(self):
        with self.loop.entered():
            def dispatch(target, method, args):
                if method == "listen":
                    return listen(args[0])
                return method

            async def listen(signal):
                self.events.append(("started", signal.cancelled))
                try:
                    await signal.wait()
                except asyncio.CancelledError:
                    self.events.append(("task cancelled", signal.cancelled))
                    return "cancelled"
                return "done"

            peer = Peer(self.channel, dispatch, endpoint=LOCAL)
            peer.receive(hello())
            peer.receive(invoke("main:1", "listen", {"type": "list", "value": [{"type": "signal"}]}))
            self.loop.run_until_idle()
            # The method is running and a future reference answers the call.
            self.assertEqual(self.events, [("started", False)])
            reference = self.channel.take(
                "the reply", lambda frame: frame["op"] == "return" and frame["id"] == "main:1"
            )["value"]
            self.assertEqual(reference["value"]["kind"], "future")
            # The cancel names the call, not the future: the session maps it
            # back to the signal and the task.
            peer.receive({"op": "cancel", "id": "main:1"})
            self.loop.run_ready()
            self.assertEqual(self.events, [("started", False), ("task cancelled", True)])
            # The task's outcome is what an await of the future would read.
            peer.receive({
                "op": "await", "id": "main:2", "path": [], "reference": reference["value"]["id"],
            })
            self.loop.run_ready()
            self.assertEqual(
                self.channel.pending(),
                [{"op": "return", "id": "main:2", "value": data("cancelled")}],
            )
            peer.close()


class SynchronousWaitTests(background.TestCase):
    """A synchronous call blocks the loop thread and pumps the frames its
    answer comes through; the test hands each frame over only after the
    previous one was answered (peer.py `_call_sync`)."""

    # guarantee: boundary/5.6
    def test_a_synchronous_call_pumps_the_frames_between_the_call_and_its_reply(self):
        channel = ScriptedChannel()
        log = []
        started = threading.Event()
        failure = []
        # What the serving thread made: the test ends them from here.
        made = {}

        def dispatch(target, method, args):
            log.append(("start", method))
            if method == "outer":
                value = made["peer"].call("host", "inner", [])
                log.append(("inner", value))
                return f"outer:{value}"
            return method

        def serve():
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            made["loop"] = loop
            try:
                async def setup():
                    peer = Peer(channel, dispatch, endpoint=LOCAL)
                    peer.start()
                    return peer

                made["peer"] = loop.run_until_complete(setup())
                started.set()
                loop.run_forever()
            except BaseException as error:  # noqa: BLE001 - reported by the test
                failure.append(error)
            finally:
                loop.close()

        thread = threading.Thread(target=serve, name="scripted runtime", daemon=True)
        thread.start()
        try:
            self.assertTrue(started.wait(HANG_GUARD), "the runtime did not start")
            # The greeting goes out first, then the far end's.
            self.assertEqual(channel.take("the greeting", lambda frame: frame["op"] == "hello")["endpoint"], "py")
            channel.feed(hello())
            # A synchronous call: its method calls back into the far end
            # before it returns, and that call's frame is the proof the loop
            # thread now waits in the pump.
            channel.feed(invoke("main:1", "outer", sync=True))
            inner = channel.take(
                "the inner call", lambda frame: frame["op"] == "invoke" and frame.get("method") == "inner"
            )
            self.assertEqual(log, [("start", "outer")], "the outer method waits for the inner answer")
            self.assertTrue(inner["sync"], "the nested call says it waits")
            channel.feed({"op": "return", "id": inner["id"], "value": data("inner-result")})
            outer = channel.take(
                "the outer answer", lambda frame: frame["op"] == "return" and frame["id"] == "main:1"
            )
            self.assertEqual(outer["value"], data("outer:inner-result"))
            self.assertEqual(log, [("start", "outer"), ("inner", "inner-result")])
            # A frame arriving after the wait is deferred like any other.
            channel.feed(invoke("main:2", "after"))
            after = channel.take(
                "the next answer", lambda frame: frame["op"] == "return" and frame["id"] == "main:2"
            )
            self.assertEqual(after["value"], data("after"))
            self.assertEqual(log[-1], ("start", "after"))
            self.assertEqual(failure, [])
        finally:
            # End the session on its loop, then the loop; a failure in the
            # test does not leave the thread running.
            loop = made.get("loop")
            if loop is not None:
                loop.call_soon_threadsafe(made["peer"].close)
                loop.call_soon_threadsafe(loop.stop)
            channel.close("the test is done")
            thread.join(HANG_GUARD)
            self.assertFalse(thread.is_alive(), "the runtime thread did not stop")
