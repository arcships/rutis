"""Managed timers (`ctx.timeout`, `ctx.interval`) in the Python runtime (#1325,
docs/design-managed-timers-2026-11-20.en.md).
"""

import asyncio
import contextlib
import io
import math
import sys
import types
import unittest

import background

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


if __name__ == "__main__":
    unittest.main()
