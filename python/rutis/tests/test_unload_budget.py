"""PoC tests for opt-in per-row unload cleanup budget & Route B taint semantics
in Python (docs/design-row-unload-budget-2026-11-20.md, #1323)."""

import asyncio
import contextlib
import io
import os
import sys
import time
import types
import unittest

import background

from rutis.peer import BindingError
from rutis.runner import Runtime, row_tainted_error_text, row_unload_deadline, row_unload_timeout_text


class Forecast:
    def __init__(self, label: str):
        self.label = label

    def today(self) -> str:
        return self.label


def module(name: str, apply) -> str:
    plugin = types.ModuleType(name)
    plugin.apply = apply
    plugin.inject = []
    sys.modules[name] = plugin
    return name


class UnloadBudget(background.TestCase):
    def test_env_parsing_matches_shutdown_timeout_convention(self):
        self.assertIsNone(row_unload_deadline({}))
        self.assertIsNone(row_unload_deadline({"RUTIS_ROW_UNLOAD_TIMEOUT": ""}))
        self.assertEqual(row_unload_deadline({"RUTIS_ROW_UNLOAD_TIMEOUT": "5"}), 5.0)
        self.assertEqual(row_unload_deadline({"RUTIS_ROW_UNLOAD_TIMEOUT": "0.25"}), 0.25)
        for invalid in ("0", "-1", "nan", "inf", "abc"):
            self.assertIsNone(row_unload_deadline({"RUTIS_ROW_UNLOAD_TIMEOUT": invalid}), invalid)

    def test_default_unset_preserves_existing_behavior(self):
        log = []

        def apply(ctx, config):
            ctx.effect(lambda: log.append("earlier"))
            ctx.provide("forecast", Forecast("v1"))

            async def delayed():
                await asyncio.sleep(0.04)
                log.append("delayed")

            ctx.effect(delayed)

        async def run():
            saved = os.environ.pop("RUTIS_ROW_UNLOAD_TIMEOUT", None)
            try:
                runtime = Runtime()
                runtime._control("mount", {"announce": True})
                await runtime.load("w", module("budget_default", apply), None, None, id="weather")
                written = io.StringIO()
                with contextlib.redirect_stderr(written):
                    await runtime.unload("w")
                return written.getvalue()
            finally:
                if saved is not None:
                    os.environ["RUTIS_ROW_UNLOAD_TIMEOUT"] = saved

        stderr = asyncio.run(run())
        self.assertEqual(log, ["delayed", "earlier"])
        self.assertEqual(stderr, "")

    def test_route_b_taints_row_until_restart_keeps_healthy_sibling_unaffected_and_isolates_late_actions(self):
        log = []
        healthy_log = []
        late_events = []
        release = None

        def apply_weather(ctx, config):
            ctx.effect(lambda: log.append("earlier"))
            withdraw = ctx.provide("forecast", Forecast(config["label"]))
            if config["hang"]:

                async def hang():
                    try:
                        await release.wait()
                    except asyncio.CancelledError:
                        await release.wait()
                    # Lingering old cleanup wakes up later and tries to mutate tables:
                    # old withdraw handle must leave sibling's service untouched,
                    # and late provide / emit / exception are safely refused / absorbed.
                    withdraw()
                    try:
                        ctx.provide("forecast", Forecast("stale"))
                    except RuntimeError as error:
                        late_events.append(f"provide:{error}")
                    try:
                        ctx.emit("weather/late")
                    except BindingError:
                        late_events.append("emit:BindingError")
                    raise RuntimeError("late cleanup boom")

                ctx.effect(hang)

        def apply_metrics(ctx, config):
            ctx.provide("forecast", Forecast("healthy-sibling"))
            ctx.effect(lambda: healthy_log.append("metrics-cleaned"))

        async def run():
            nonlocal release
            release = asyncio.Event()
            runtime = Runtime(row_unload_timeout=0.05)
            runtime._control("mount", {"announce": True})
            weather_mod = module("budget_route_b_weather", apply_weather)
            metrics_mod = module("budget_route_b_metrics", apply_metrics)
            await runtime.load("w1", weather_mod, {"label": "v1", "hang": True}, None, id="weather")
            self.assertIn("forecast", runtime.services)
            written = io.StringIO()
            started = time.monotonic()
            with contextlib.redirect_stderr(written):
                await runtime.unload("w1")
            elapsed = time.monotonic() - started
            self.assertLess(elapsed, 1.0, "unload returned promptly after budget")
            self.assertEqual(runtime.services, {}, "provided service was withdrawn")
            self.assertEqual(runtime.rows, {}, "row left runtime table")

            # Route B: reloading the tainted row in this runtime (same key or fresh key) fails explicitly.
            expected_taint = row_tainted_error_text("py", "weather", 0.05)
            self.assertEqual(
                expected_taint,
                "row weather cannot be loaded in runtime py: its previous cleanup did not finish within 0.05 s "
                "and may still be running; restart the runtime (or isolate this row in its own runtime) "
                "before loading it again",
            )
            with self.assertRaises(RuntimeError) as same_key:
                await runtime.load("w1", weather_mod, {"label": "v2", "hang": False}, None, id="weather")
            self.assertEqual(str(same_key.exception), expected_taint)
            with self.assertRaises(RuntimeError) as new_key:
                await runtime.load("w2", weather_mod, {"label": "v2", "hang": False}, None, id="weather")
            self.assertEqual(str(new_key.exception), expected_taint)

            # Unrelated healthy sibling loads, takes over the withdrawn service, and is untouched by late actions.
            await runtime.load("m1", metrics_mod, {}, None, id="metrics")
            self.assertEqual(runtime.lookup("forecast").today(), "healthy-sibling")
            release.set()
            await asyncio.sleep(0.01)
            self.assertEqual(late_events, ["provide:row weather is not active", "emit:BindingError"])
            self.assertEqual(runtime.lookup("forecast").today(), "healthy-sibling")
            await runtime.unload("m1")
            self.assertEqual(healthy_log, ["metrics-cleaned"])

            # Fresh runtime (after restart) clears taint and loads weather cleanly.
            restarted = Runtime(row_unload_timeout=0.05)
            restarted._control("mount", {"announce": True})
            await restarted.load("w1", weather_mod, {"label": "v2", "hang": False}, None, id="weather")
            self.assertEqual(restarted.lookup("forecast").today(), "v2")
            await restarted.unload("w1")
            return written.getvalue()

        stderr = asyncio.run(run())
        expected = f"{row_unload_timeout_text('py', 'weather', 0.05)}\n"
        self.assertEqual(
            expected,
            "runtime py: cleanup of row weather did not finish within 0.05 s; "
            "skipped remaining cleanups and withdrew its services; "
            "reloading row weather in this runtime is blocked until the runtime restarts "
            "(the stuck cleanup may still be running; fix it or restart the runtime)\n",
        )
        self.assertEqual(stderr, expected)
        self.assertEqual(log, ["earlier"], "earlier skipped on timeout, ran cleanly after runtime restart")

    def test_uncooperative_coroutine_via_env_var_times_out_taints_and_withdraws(self):
        # Proves we use asyncio.wait + cancel(), not asyncio.wait_for (which
        # blocks forever if a coroutine catches CancelledError and keeps waiting).
        log = []
        release = None

        def apply(ctx, config):
            ctx.effect(lambda: log.append("earlier"))
            ctx.provide("forecast", Forecast("stubborn"))

            async def stubborn():
                try:
                    await release.wait()
                except asyncio.CancelledError:
                    await release.wait()

            ctx.effect(stubborn)

        async def run():
            nonlocal release
            release = asyncio.Event()
            saved = os.environ.get("RUTIS_ROW_UNLOAD_TIMEOUT")
            os.environ["RUTIS_ROW_UNLOAD_TIMEOUT"] = "0.05"
            try:
                runtime = Runtime()
                runtime._control("mount", {"announce": True})
                mod = module("budget_stubborn", apply)
                await runtime.load("w", mod, None, None, id="weather")
                written = io.StringIO()
                started = time.monotonic()
                with contextlib.redirect_stderr(written):
                    await runtime.unload("w")
                elapsed = time.monotonic() - started
                self.assertLess(elapsed, 1.0, "uncooperative coroutine did not block unload")
                self.assertEqual(runtime.services, {}, "provided service still withdrawn")
                with self.assertRaisesRegex(RuntimeError, "^row weather cannot be loaded in runtime py:"):
                    await runtime.load("w2", mod, None, None, id="weather")
                # Let the abandoned task exit cleanly before the event loop closes.
                release.set()
                await asyncio.sleep(0)
                return written.getvalue()
            finally:
                if saved is None:
                    os.environ.pop("RUTIS_ROW_UNLOAD_TIMEOUT", None)
                else:
                    os.environ["RUTIS_ROW_UNLOAD_TIMEOUT"] = saved

        stderr = asyncio.run(run())
        self.assertEqual(
            stderr,
            "runtime py: cleanup of row weather did not finish within 0.05 s; "
            "skipped remaining cleanups and withdrew its services; "
            "reloading row weather in this runtime is blocked until the runtime restarts "
            "(the stuck cleanup may still be running; fix it or restart the runtime)\n",
        )
        self.assertEqual(log, [])

    def test_pre_timeout_error_is_preserved_while_subsequent_hang_times_out_and_withdraws(self):
        log = []

        def apply(ctx, config):
            ctx.effect(lambda: log.append("earliest"))
            ctx.provide("forecast", Forecast("err"))

            async def hang():
                await asyncio.Event().wait()

            def boom():
                raise RuntimeError("flush failed")

            ctx.effect(hang)
            ctx.effect(boom)

        async def run():
            runtime = Runtime(row_unload_timeout=0.05)
            runtime._control("mount", {"announce": True})
            await runtime.load("w", module("budget_err_then_hang", apply), None, None, id="weather")
            written = io.StringIO()
            with contextlib.redirect_stderr(written):
                with self.assertRaisesRegex(RuntimeError, "^flush failed$"):
                    await runtime.unload("w")
            self.assertEqual(runtime.services, {}, "provided service still withdrawn")
            return written.getvalue()

        stderr = asyncio.run(run())
        self.assertEqual(
            stderr,
            "runtime py: cleanup of row weather did not finish within 0.05 s; "
            "skipped remaining cleanups and withdrew its services; "
            "reloading row weather in this runtime is blocked until the runtime restarts "
            "(the stuck cleanup may still be running; fix it or restart the runtime)\n",
        )
        self.assertEqual(log, [])


if __name__ == "__main__":
    unittest.main()
