"""Managed timers (`ctx.timeout`, `ctx.interval`) in the Python runtime (#1325,
docs/design-managed-timers-2026-11-20.en.md).
"""

import asyncio
import contextlib
import io
import math
import sys
import threading
import types
import unittest

import background

from rutis._unhandled import current_row
from rutis.runner import Runtime


def module(name, apply) -> str:
    plugin = types.ModuleType(name)
    plugin.apply = apply
    sys.modules[name] = plugin
    return name


class ManagedTimers(background.TestCase):
    def runtime(self):
        runtime = Runtime()
        runtime._control("mount", {"announce": True})
        return runtime

    def test_timeout_fires_once_and_respects_early_cancel_and_unload(self):
        fired = []
        cancelled_fired = []
        unloaded_fired = []

        def apply_ok(ctx, config):
            ctx.timeout(0.015, lambda: fired.append("ok"))
            cancel = ctx.timeout(0.015, lambda: cancelled_fired.append("no"))
            cancel()
            cancel()  # idempotent

        def apply_unload(ctx, config):
            ctx.timeout(0.04, lambda: unloaded_fired.append("no"))

        async def run():
            rt = self.runtime()
            await rt.load("ok", module("timers_ok", apply_ok), None, None)
            await asyncio.sleep(0.05)
            await rt.unload("ok")

            await rt.load("u", module("timers_unload", apply_unload), None, None)
            await rt.unload("u")
            await asyncio.sleep(0.07)

        asyncio.run(run())
        self.assertEqual(fired, ["ok"])
        self.assertEqual(cancelled_fired, [])
        self.assertEqual(unloaded_fired, [])

    def test_interval_fires_multiple_times_stops_on_unload_non_overlapping_and_self_cancel(self):
        active = 0
        max_active = 0
        ticks = 0
        reached = None

        def apply_loop(ctx, config):
            async def tick():
                nonlocal active, max_active, ticks
                active += 1
                if active > max_active:
                    max_active = active
                await asyncio.sleep(0.03)
                active -= 1
                ticks += 1
                if ticks >= 3:
                    reached.set()

            ctx.interval(0.01, tick)

        self_ticks = []

        def apply_self_cancel(ctx, config):
            stop_holder = {}

            def tick():
                self_ticks.append(len(self_ticks) + 1)
                if len(self_ticks) == 2:
                    stop_holder["stop"]()

            stop_holder["stop"] = ctx.interval(0.01, tick)

        async def run():
            nonlocal reached
            reached = asyncio.Event()
            rt = self.runtime()
            await rt.load("hb", module("timers_loop", apply_loop), None, None)
            await asyncio.wait_for(reached.wait(), timeout=2.0)
            await rt.unload("hb")
            at_unload = ticks
            await asyncio.sleep(0.06)
            self.assertEqual(ticks, at_unload, "zero fires after unload")
            self.assertGreaterEqual(at_unload, 3)
            self.assertEqual(max_active, 1, "slow async callbacks must not overlap")

            await rt.load("self", module("timers_self", apply_self_cancel), None, None)
            await asyncio.sleep(0.08)
            await rt.unload("self")

        asyncio.run(run())
        self.assertEqual(self_ticks, [1, 2])

    def test_registering_during_unload_and_after_dispose_is_refused_verbatim(self):
        written = io.StringIO()
        ran = []
        holder = {}

        def apply(ctx, config):
            holder["ctx"] = ctx

            def cleanup():
                c1 = ctx.timeout(0.001, lambda: ran.append("t"))
                c2 = ctx.interval(0.001, lambda: ran.append("i"))
                c1()
                c2()

            ctx.effect(cleanup)

        async def run():
            rt = self.runtime()
            await rt.load("heartbeat", module("timers_late", apply), None, None, id="heartbeat")
            with contextlib.redirect_stderr(written):
                await rt.unload("heartbeat")
                holder["ctx"].timeout(0.001, lambda: ran.append("after"))
            await asyncio.sleep(0.03)

        asyncio.run(run())
        self.assertEqual(ran, [])
        self.assertEqual(
            written.getvalue(),
            "rutis: row heartbeat is not active: timer is not registered\n" * 3,
        )

    def test_invalid_arguments_raise_exact_errors(self):
        errors = {}

        def apply(ctx, config):
            def check(name, fn):
                try:
                    fn()
                except Exception as exc:  # noqa: BLE001
                    errors[name] = (type(exc).__name__, str(exc))

            check("neg_timeout", lambda: ctx.timeout(-1, lambda: None))
            check("nan_timeout", lambda: ctx.timeout(math.nan, lambda: None))
            check("bool_timeout", lambda: ctx.timeout(True, lambda: None))
            check("zero_interval", lambda: ctx.interval(0, lambda: None))
            check("neg_interval", lambda: ctx.interval(-0.5, lambda: None))
            check("bad_timeout_cb", lambda: ctx.timeout(0.1, "nope"))
            check("bad_interval_cb", lambda: ctx.interval(0.1, 123))

        async def run():
            rt = self.runtime()
            await rt.load("bad", module("timers_bad", apply), None, None)
            await rt.unload("bad")

        asyncio.run(run())
        self.assertEqual(
            errors["neg_timeout"],
            ("ValueError", "timeout delay must be a non-negative finite number of seconds, got -1"),
        )
        self.assertEqual(
            errors["nan_timeout"],
            ("ValueError", "timeout delay must be a non-negative finite number of seconds, got nan"),
        )
        self.assertEqual(
            errors["bool_timeout"],
            ("ValueError", "timeout delay must be a non-negative finite number of seconds, got True"),
        )
        self.assertEqual(
            errors["zero_interval"],
            ("ValueError", "interval period must be a positive finite number of seconds, got 0"),
        )
        self.assertEqual(
            errors["neg_interval"],
            ("ValueError", "interval period must be a positive finite number of seconds, got -0.5"),
        )
        self.assertEqual(errors["bad_timeout_cb"], ("TypeError", "timeout needs a callable callback"))
        self.assertEqual(errors["bad_interval_cb"], ("TypeError", "interval needs a callable callback"))

    def test_callback_errors_are_contained_reported_verbatim_and_interval_continues(self):
        written = io.StringIO()
        interval_ticks = 0
        reached = None
        unhandled = []

        def apply(ctx, config):
            def sync_fail():
                raise RuntimeError("sync boom")

            async def async_fail():
                raise RuntimeError("connection refused")

            async def tick():
                nonlocal interval_ticks
                interval_ticks += 1
                if interval_ticks == 1:
                    raise ValueError("tick one failed")
                if interval_ticks >= 3:
                    reached.set()

            ctx.timeout(0.005, sync_fail)
            ctx.timeout(0.005, async_fail)
            ctx.interval(0.01, tick)

        async def run():
            nonlocal reached
            reached = asyncio.Event()
            loop = asyncio.get_running_loop()
            loop.set_exception_handler(lambda _loop, context: unhandled.append(context))
            rt = self.runtime()
            # Keep our custom unhandled detector on top of the runtime's to prove nothing escapes.
            loop.set_exception_handler(lambda _loop, context: unhandled.append(context))
            with contextlib.redirect_stderr(written):
                await rt.load("heartbeat", module("timers_faulty", apply), None, None, id="heartbeat")
                await asyncio.wait_for(reached.wait(), timeout=2.0)
                await rt.unload("heartbeat")

        asyncio.run(run())
        output = written.getvalue()
        self.assertEqual(unhandled, [], "no exception should escape to the loop handler")
        self.assertGreaterEqual(interval_ticks, 3)
        self.assertIn("rutis: timer of row heartbeat failed: RuntimeError: sync boom\n", output)
        self.assertIn("rutis: timer of row heartbeat failed: RuntimeError: connection refused\n", output)
        self.assertIn("rutis: timer of row heartbeat failed: ValueError: tick one failed\n", output)

    def test_self_cancel_before_await_still_awaits_task_exit_before_earlier_lifo_cleanup(self):
        for mode in ("timeout", "interval"):
            with self.subTest(mode=mode):
                events = []
                pool_open = True
                started = None
                release = None

                def apply(ctx, config):
                    nonlocal pool_open
                    cancel_holder = {}

                    def close_pool():
                        nonlocal pool_open
                        pool_open = False
                        events.append("pool.close")

                    ctx.effect(close_pool)

                    async def tick():
                        # Synchronously self-cancel BEFORE the first await.
                        cancel_holder["cancel"]()
                        events.append("callback.started")
                        started.set()
                        try:
                            await release.wait()
                        finally:
                            events.append("callback.exited-open-pool" if pool_open else "callback.USED-CLOSED-POOL")

                    cancel_holder["cancel"] = getattr(ctx, mode)(0.005, tick)

                async def run():
                    nonlocal started, release
                    started = asyncio.Event()
                    release = asyncio.Event()
                    rt = self.runtime()
                    key = f"sc_{mode}"
                    await rt.load(key, module(f"timers_{key}", apply), None, None)
                    await asyncio.wait_for(started.wait(), timeout=2.0)
                    # Unload cancels the in-flight task and waits for its finally
                    # block to exit BEFORE running the earlier pool.close() effect.
                    await rt.unload(key)

                asyncio.run(run())
                self.assertEqual(events, ["callback.started", "callback.exited-open-pool", "pool.close"])

    def test_timer_registered_and_cancelled_from_worker_thread(self):
        fired = []
        cancelled_fired = []
        row_seen = []
        errors = []
        interval_ticks = 0

        async def run():
            nonlocal interval_ticks
            done = asyncio.Event()
            interval_ready = asyncio.Event()
            ctx_holder = {}

            def apply(ctx, config):
                ctx_holder["ctx"] = ctx

            rt = self.runtime()
            await rt.load("worker_row", module("timers_worker", apply), None, None, id="worker_row")
            ctx = ctx_holder["ctx"]
            row = rt.rows["worker_row"]

            def worker():
                try:
                    def on_fire():
                        row_seen.append(current_row.get())
                        fired.append(threading.current_thread().ident)
                        done.set()

                    def on_tick():
                        nonlocal interval_ticks
                        interval_ticks += 1
                        if interval_ticks >= 2:
                            interval_ready.set()

                    ctx.timeout(0.015, on_fire)
                    stop_timeout = ctx.timeout(0.05, lambda: cancelled_fired.append("no"))
                    stop_timeout()
                    stop_timeout()  # idempotent from foreign thread
                    ctx.interval(0.01, on_tick)
                except BaseException as exc:  # noqa: BLE001
                    errors.append(exc)

            thread = threading.Thread(target=worker, name="timer-worker")
            thread.start()
            await asyncio.to_thread(thread.join)

            await asyncio.wait_for(done.wait(), timeout=2.0)
            await asyncio.wait_for(interval_ready.wait(), timeout=2.0)
            await asyncio.sleep(0.07)
            # Only the active interval cleanup should remain on row.steps; both
            # the completed timeout and the cancelled timeout must have detached.
            self.assertEqual(len(row.steps), 1)
            await rt.unload("worker_row")
            ticks_at_unload = interval_ticks
            await asyncio.sleep(0.04)
            self.assertEqual(interval_ticks, ticks_at_unload, "interval from worker thread stops on unload")

            # Registering from a worker thread after unload is refused verbatim.
            late_fired = []
            written = io.StringIO()
            with contextlib.redirect_stderr(written):
                t2 = threading.Thread(
                    target=lambda: ctx.timeout(0.005, lambda: late_fired.append("late")),
                    name="timer-worker-late",
                )
                t2.start()
                await asyncio.to_thread(t2.join)
                await asyncio.sleep(0.03)
            self.assertEqual(late_fired, [])
            self.assertEqual(
                written.getvalue(),
                "rutis: row worker_row is not active: timer is not registered\n",
            )

        asyncio.run(run())
        self.assertEqual(errors, [])
        self.assertEqual(fired, [threading.current_thread().ident])
        self.assertEqual(row_seen, ["worker_row"])
        self.assertEqual(cancelled_fired, [])
        self.assertGreaterEqual(interval_ticks, 2)

    def test_worker_thread_timer_registration_racing_unload_is_safe(self):
        fired_after_unload = []
        errors = []
        unloaded = False

        async def run():
            nonlocal unloaded
            ctx_holder = {}

            def apply(ctx, config):
                ctx_holder["ctx"] = ctx

            rt = self.runtime()
            await rt.load("race_row", module("timers_race", apply), None, None, id="race_row")
            ctx = ctx_holder["ctx"]
            started = threading.Event()
            written = io.StringIO()

            def worker():
                try:
                    started.set()
                    for _ in range(30):
                        ctx.timeout(0.06, lambda: fired_after_unload.append(unloaded))
                except BaseException as exc:  # noqa: BLE001
                    errors.append(exc)

            with contextlib.redirect_stderr(written):
                thread = threading.Thread(target=worker, name="timer-race-worker")
                thread.start()
                await asyncio.to_thread(started.wait)
                await rt.unload("race_row")
                unloaded = True
                await asyncio.to_thread(thread.join)
                await asyncio.sleep(0.1)

        asyncio.run(run())
        self.assertEqual(errors, [])
        self.assertEqual(fired_after_unload, [], "no worker-registered timer may fire after unload")


if __name__ == "__main__":
    unittest.main()
