"""The e2e probe for the Python runtime: calls the services the harness asks
for and prints each result as one JSON line on stdout.

The harness writes a request as `<seq>.json` into `config["dir"]`:
`{"service": "report", "method": "summary", "args": ["greeting"]}`. The
probe answers `{"probe": "<id>", "seq": <seq>, "ok": <result>}` or
`{"probe": "<id>", "seq": <seq>, "error": "<message>"}`, and reports
`{"probe": "<id>", "event": "started"}` / `"stopped"` as it loads and
unloads. A request it cannot read is reported as
`{"probe": "<id>", "failed": "<message>"}`, and it goes on polling. The harness writes one copy per probe, with the services it calls
in place of `INJECT` (a plugin declares what it uses), so it starts once
they run.
"""

import asyncio
import inspect
import json
import os

inject = INJECT


def say(id, **record):
    print(json.dumps({"probe": id, **record}), flush=True)


async def apply(ctx, config):
    id, folder = config["id"], config["dir"]

    async def call(seq, request):
        try:
            service = ctx.use(request["service"])
            result = getattr(service, request["method"])(*request["args"])
            if inspect.isawaitable(result):
                result = await result
            say(id, seq=seq, ok=result)
        except Exception as error:  # reported to the harness, which decides
            say(id, seq=seq, error=str(error))

    async def poll():
        while True:
            try:
                names = sorted(
                    (name for name in os.listdir(folder) if name.endswith(".json")),
                    key=lambda name: int(name[: -len(".json")]),
                )
                for name in names:
                    path = os.path.join(folder, name)
                    with open(path, encoding="utf-8") as file:
                        request = json.load(file)
                    os.remove(path)
                    await call(int(name[: -len(".json")]), request)
            except Exception as error:  # the harness fails the scenario on it
                say(id, failed=f"{type(error).__name__}: {error}")
            await asyncio.sleep(0.05)

    task = asyncio.get_running_loop().create_task(poll())
    say(id, event="started")

    def stop():
        task.cancel()
        say(id, event="stopped")

    ctx.effect(stop)
