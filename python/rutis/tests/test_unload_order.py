"""Unloading runs one stack in reverse: cleanups and the withdrawals of the
services a row provides, in the order they were registered (Rust's
drain_effects; docs/design-unload-order-2026-11-01.md).

guarantee: sdk/unload-order
"""

import asyncio
import contextlib
import io
import sys
import types
import unittest

import background

from rutis.runner import Runtime


class Tally:
    def __init__(self, n):
        self.n = n

    def count(self):
        return self.n


def module(name, apply, inject=()) -> str:
    """Register a plugin module `name` whose `apply` is `apply`."""
    plugin = types.ModuleType(name)
    plugin.apply = apply
    plugin.inject = list(inject)
    sys.modules[name] = plugin
    return name


def read(ctx, name="tally") -> str:
    """What reading `name` through `ctx` gives: the count, or the error."""
    try:
        return f"count {ctx.use(name).count()}"
    except Exception as error:  # noqa: BLE001 - what the cleanup saw
        return f"error {getattr(error, 'code', type(error).__name__)}"


class Told:
    """The frames a host would hear, recorded: `refresh` hands the `service`
    frames it would send here instead of to a session."""

    def __init__(self, loop):
        self.loop = loop
        self.frames = []
        self.closed_error = None

    def notify(self, target, method, args):
        self.frames.append({"target": target, "method": method, "args": args})

    def call_async(self, target, method, args):
        self.frames.append({"target": target, "method": method, "args": args})
        # Sent at once: the test reads what was told without stepping the
        # loop again (a real session answers later).
        done = self.loop.create_future()
        done.set_result(None)
        return done


class Order(background.TestCase):
    def runtime(self):
        runtime = Runtime()
        runtime._control("mount", {"announce": True})
        return runtime

    def test_a_cleanup_after_provide_reads_it_and_one_before_does_not(self):
        log = []

        def apply(ctx, config):
            ctx.effect(lambda: log.append(f"before: {read(ctx)}"))
            ctx.provide("tally", Tally(7))
            ctx.effect(lambda: log.append(f"after: {read(ctx)}"))

        async def run():
            runtime = self.runtime()
            await runtime.load("o", module("order_around", apply), None, None)
            await runtime.unload("o")

        asyncio.run(run())
        self.assertEqual(log, ["after: count 7", "before: error not-declared"])

    def test_another_row_loses_it_when_the_unload_starts(self):
        log = []
        holder = {}

        def provider(ctx, config):
            ctx.provide("tally", Tally(2))

            # Registered after the provide: it runs before the withdrawal, and
            # at that same moment the other row no longer sees the service.
            # Reading it only after the unload would not prove this: by then
            # the registration is gone for everyone.
            def during():
                log.append(f"own: {read(ctx)}")
                log.append(f"other: {read(holder['ctx'])}")

            ctx.effect(during)

        def user(ctx, config):
            holder["ctx"] = ctx

        async def run():
            runtime = self.runtime()
            await runtime.load("p", module("loser_provider", provider), None, None)
            await runtime.load("u", module("loser_user", user, inject=["tally"]), None, None)
            log.append(f"before: {read(holder['ctx'])}")
            await runtime.unload("p")
            log.append(f"after: {read(holder['ctx'])}")

        asyncio.run(run())
        self.assertEqual(
            log,
            [
                "before: count 2",
                "own: count 2",
                "other: error unavailable",
                "after: error unavailable",
            ],
        )

    def test_a_cleanup_registering_one_is_refused(self):
        written = io.StringIO()
        ran = []

        def apply(ctx, config):
            def outer():
                ctx.effect(lambda: ran.append("inner"))

            ctx.effect(outer)

        async def run():
            runtime = self.runtime()
            await runtime.load("nested", module("nested_cleanup", apply), None, None)
            with contextlib.redirect_stderr(written):
                await runtime.unload("nested")

        asyncio.run(run())
        self.assertEqual(ran, [])
        self.assertEqual(written.getvalue(), "rutis: row nested is not active: cleanup is not registered\n")

    def test_an_old_withdrawal_leaves_a_new_registration_alone(self):
        log = []

        def apply(ctx, config):
            withdraw = ctx.provide("tally", Tally(5))
            withdraw()
            ctx.provide("tally", Tally(5))
            # The first registration's withdrawal is on the stack too; it
            # removes nothing now.
            log.append(f"during: {read(ctx)}")
            ctx.effect(lambda: log.append(f"cleanup: {read(ctx)}"))

        async def run():
            runtime = self.runtime()
            await runtime.load("a", module("again", apply), None, None)
            await runtime.unload("a")

        asyncio.run(run())
        self.assertEqual(log, ["during: count 5", "cleanup: count 5"])

    def test_the_service_is_gone_afterwards(self):
        def apply(ctx, config):
            ctx.provide("tally", Tally(1))

        async def run():
            runtime = self.runtime()
            await runtime.load("g", module("gone_after", apply), None, None)
            self.assertIn("tally", runtime.services)
            await runtime.unload("g")
            self.assertNotIn("tally", runtime.services)

        asyncio.run(run())

    def test_a_service_provided_after_the_unload_started_is_refused(self):
        # A row on its unload stack registers nothing any more (#1143): its
        # withdrawal would join a stack the unload had already taken, so it
        # would never run and the service would stay in the table with no
        # row, and the host would be told about it by `refresh` while it is
        # being told that this row's services are gone.
        started, release = asyncio.Event(), asyncio.Event()
        holder = {}

        def apply(ctx, config):
            holder["ctx"] = ctx

            async def cleanup():
                started.set()
                await release.wait()

            ctx.provide("kept", Tally(4))
            ctx.effect(cleanup)

        async def run():
            runtime = self.runtime()
            runtime.peer = Told(asyncio.get_running_loop())
            await runtime.load("late", module("late_provide", apply), None, None)
            ctx = holder["ctx"]
            unloading = asyncio.create_task(runtime.unload("late"))
            # The cleanup waits: the unload is inside its stack, past the
            # point where it took it.
            await started.wait()
            self.assertTrue(runtime.rows["late"].unloading, "the row is on its unload stack")

            with self.assertRaises(RuntimeError) as refused:
                ctx.provide("late", Tally(9))
            self.assertEqual(str(refused.exception), "row late is not active")
            self.assertEqual(list(runtime.services), ["kept"], "nothing new was provided")
            self.assertNotIn("late", runtime.slots, "the host has no slot to hear about")

            release.set()
            await unloading
            # The same once the row has gone.
            with self.assertRaises(RuntimeError):
                ctx.provide("late", Tally(9))
            self.assertEqual(runtime.services, {}, "the row's own service was withdrawn")
            # `kept` was announced and withdrawn, as before; `late` was
            # never a frame.
            self.assertEqual([frame["args"][0] for frame in runtime.peer.frames], ["kept", "kept"])
            self.assertIsNone(runtime.peer.frames[1]["args"][1], "the withdrawal says it is gone")
            self.assertIn("count", runtime.peer.frames[0]["args"][3]["methods"], "the announcement still says its methods")

        asyncio.run(run())


if __name__ == "__main__":
    unittest.main()
