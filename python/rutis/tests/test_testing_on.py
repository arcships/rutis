"""rutis.testing: the test listens on the host (`t.on`, #670 C4).

The host starts an `emit`'s invocations on a task of its own and answers the
dispatch once they started (events protocol §5.5), so a round trip through
the host after an emission of a plugin's is no barrier for it: where a test
needs the listener to have run, it waits for the listener.
"""

import asyncio
import unittest

import background
from support import within

from rutis import define_plugin
from rutis.testing import Bus, PluginTestError, load


class Weather:
    def __init__(self, ctx, city):
        self.ctx, self.city = ctx, city

    async def today(self):
        self.ctx.emit("weather/asked", self.city)
        return "sunny"

    async def ask(self, place):
        return await self.ctx.serial("weather/answer", place)

    async def tell(self, entry):
        self.ctx.emit("weather/told", entry)
        return entry["city"]


def weather(service="weather"):
    def apply(ctx, config):
        ctx.provide(service, Weather(ctx, config["city"]))

    return define_plugin(apply, name="weather")


def run(coroutine):
    return asyncio.run(coroutine)


class On(background.TestCase):
    def test_it_hears_the_plugins_emissions(self):
        async def go():
            async with load(weather(), config={"city": "Oslo"}) as t:
                asked, heard = [], asyncio.Event()

                def hear(city):
                    asked.append(city)
                    heard.set()

                off = t.on("weather/asked", hear)
                await t.service("weather").today()
                # The host starts an emit's invocations on a task of its own
                # (events protocol §5.5): a round trip after the emit may be
                # answered before the listener ran, so wait for the listener.
                await within("the plugin's emission to reach the test", heard.wait())
                self.assertEqual(asked, ["Oslo"])
                off()

        run(go())

    def test_it_hears_the_tests_own_emissions(self):
        async def go():
            async with load(weather(), config={"city": "Oslo"}) as t:
                heard, first = [], asyncio.Event()

                def hear(city, n):
                    heard.append((city, n))
                    first.set()

                t.on("weather/asked", hear)
                t.emit("weather/asked", "Bergen", 2)
                # An emit and a dispatch are not ordered against each other
                # (§5.5), so the next emission waits for this one: heard in
                # emission order is then what the assertions below check.
                await within("the test's own emit to reach its listener", first.wait())
                await t.parallel("weather/asked", "Tromsø", 3)
                self.assertEqual(heard, [("Bergen", 2), ("Tromsø", 3)])

        run(go())

    def test_it_gets_the_arguments_as_the_host_sends_them(self):
        # objects-case(python): data-stays-data
        async def go():
            entry = {"city": "Oslo", "tags": ("a", "b")}
            got, heard = [], asyncio.Event()

            def hear(received):
                got.append(received)
                heard.set()

            async with load(weather(), config={"city": "Oslo"}) as t:
                t.on("weather/told", hear)
                await t.service("weather").tell(entry)
                await within("the plugin's emission to reach the test", heard.wait())
            # A copy, a tuple arriving as a list: the event went through the host.
            self.assertIsNot(got[0], entry)
            self.assertEqual(got[0], {"city": "Oslo", "tags": ["a", "b"]})

        run(go())

    def test_the_returned_function_stops_it(self):
        async def go():
            async with load(weather(), config={"city": "Oslo"}) as t:
                asked = []
                off = t.on("weather/asked", asked.append)
                off()
                off()
                await t.service("weather").today()
                await t.parallel("weather/sync")
                self.assertEqual(asked, [])

        run(go())

    def test_it_stops_when_the_plugin_unloads(self):
        async def go():
            bus = Bus()
            asked = []
            async with load(weather("forecast"), config={"city": "Bergen"}, bus=bus, id="second") as other:
                async with load(weather(), config={"city": "Oslo"}, bus=bus) as t:
                    t.on("weather/asked", asked.append)
                await other.service("forecast").today()
                await other.parallel("weather/sync")
                self.assertEqual(asked, [])
                with self.assertRaisesRegex(PluginTestError, "the plugin is unloaded"):
                    t.on("weather/asked", asked.append)

        run(go())

    def test_in_serial_its_result_counts(self):
        async def go():
            async with load(weather(), config={"city": "Oslo"}) as t:

                async def answer(place):
                    return f"rain in {place}"

                t.on("weather/answer", answer)
                self.assertEqual(await t.service("weather").ask("Oslo"), "rain in Oslo")
                # Options as ctx.on: a prepended listener is asked first.
                t.on("weather/answer", lambda place: f"snow in {place}", prepend=True)
                self.assertEqual(await t.service("weather").ask("Oslo"), "snow in Oslo")

        run(go())

    def test_update_keeps_it(self):
        async def go():
            async with load(weather(), config={"city": "Oslo"}) as t:
                asked, heard = [], asyncio.Event()

                def hear(city):
                    asked.append(city)
                    heard.set()

                t.on("weather/asked", hear)
                await t.update({"city": "Bergen"})
                await t.service("weather").today()
                # A round trip after the emit does not wait for it (§5.5),
                # before or after the update: wait for the listener.
                await within("the updated plugin's emission to reach the test", heard.wait())
                self.assertEqual(asked, ["Bergen"])

        run(go())


if __name__ == "__main__":
    unittest.main()
