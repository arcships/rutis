"""PoC tests for opt-in per-row unload cleanup budget in Python
(docs/design-row-unload-budget-2026-11-20.md, #1323)."""

import asyncio
import contextlib
import io
import os
import sys
import time
import types
import unittest

import background

from rutis.runner import Runtime, row_unload_deadline, row_unload_timeout_text


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

    def test_opt_in_budget_times_out_skips_later_cleanups_and_withdraws_services(self):
        log = []

        def apply(ctx, config):
            ctx.effect(lambda: log.append("earlier"))
            ctx.provide("forecast", Forecast(config["label"]))
            if config["hang"]:

                async def hang():
                    await asyncio.Event().wait()

                ctx.effect(hang)

        async def run():
            runtime = Runtime(row_unload_timeout=0.05)
            runtime._control("mount", {"announce": True})
            mod = module("budget_hang", apply)
            await runtime.load("w", mod, {"label": "v1", "hang": True}, None, id="weather")
            self.assertIn("forecast", runtime.services)
            written = io.StringIO()
            started = time.monotonic()
            with contextlib.redirect_stderr(written):
                await runtime.unload("w")
            elapsed = time.monotonic() - started
            self.assertLess(elapsed, 1.0, "unload returned promptly after budget")
            self.assertEqual(runtime.services, {}, "provided service was withdrawn")
            self.assertEqual(runtime.rows, {}, "row left runtime table")
            # Reloading the same row and service name succeeds cleanly.
            await runtime.load("w", mod, {"label": "v2", "hang": False}, None, id="weather")
            self.assertEqual(runtime.lookup("forecast").today(), "v2")
            await runtime.unload("w")
            return written.getvalue()

        stderr = asyncio.run(run())
        expected = f"{row_unload_timeout_text('py', 'weather', 0.05)}\n"
        self.assertEqual(
            expected,
            "runtime py: cleanup of row weather did not finish within 0.05 s; "
            "skipped remaining cleanups and withdrew its services "
            "(the stuck cleanup may still be running; fix it or restart the runtime)\n",
        )
        self.assertEqual(stderr, expected)
        self.assertEqual(log, ["earlier"], "earlier skipped on timeout, ran on clean reload unload")

    def test_uncooperative_coroutine_trapping_cancelled_error_still_times_out_and_withdraws(self):
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
                await runtime.load("w", module("budget_stubborn", apply), None, None, id="weather")
                written = io.StringIO()
                started = time.monotonic()
                with contextlib.redirect_stderr(written):
                    await runtime.unload("w")
                elapsed = time.monotonic() - started
                self.assertLess(elapsed, 1.0, "uncooperative coroutine did not block unload")
                self.assertEqual(runtime.services, {}, "provided service still withdrawn")
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
            "skipped remaining cleanups and withdrew its services "
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
            "skipped remaining cleanups and withdrew its services "
            "(the stuck cleanup may still be running; fix it or restart the runtime)\n",
        )
        self.assertEqual(log, [])


if __name__ == "__main__":
    unittest.main()
