"""The session layer against a scripted peer on a socketpair."""

import asyncio
import json
import socket
import threading
import unittest

import background

from rutis.peer import Peer


class PeerTests(background.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.ours, self.theirs = socket.socketpair()
        self.theirs.settimeout(5)
        self.peer = Peer(self.ours, lambda target, method, args: None)
        self.peer._handshake = True
        self.lines = self.theirs.makefile("rb")

    async def asyncTearDown(self):
        self.peer.close()
        self.ours.close()
        self.theirs.close()

    def read(self):
        return json.loads(self.lines.readline())

    async def test_a_finished_future_is_ready_during_a_synchronous_call(self):
        loop = asyncio.get_running_loop()
        future = loop.create_future()
        future.set_result(42)
        # Exported, but its done-callback is still only scheduled.
        reference = self.peer._encode(future, [], False)["value"]["id"]
        # Inside a synchronous call `node:1`, Rust awaits it on that chain.
        self.peer._waiting.append("node:1")
        self.peer._receive({"op": "await", "id": "rust:1", "path": ["node:1"], "reference": reference})
        self.peer._waiting.pop()
        reply = self.read()
        self.assertEqual(reply["op"], "return", reply)
        self.assertEqual(reply["value"], {"type": "data", "value": 42})

    async def test_a_pending_future_on_the_chain_is_a_cycle(self):
        future = asyncio.get_running_loop().create_future()
        reference = self.peer._encode(future, [], False)["value"]["id"]
        self.peer._waiting.append("node:1")
        self.peer._receive({"op": "await", "id": "rust:1", "path": ["node:1"], "reference": reference})
        self.peer._waiting.pop()
        reply = self.read()
        self.assertEqual(reply["op"], "throw")
        self.assertEqual(reply["error"]["name"], "SyncWaitCycle")
        future.cancel()



class MalformedFrameTests(unittest.IsolatedAsyncioTestCase):
    """Q6.2.3, risk P3: a frame of the wrong shape, or naming a call or
    reference this side does not have, ends the session; the cases of
    rutis-bridge's `malformed_and_dangling_frames_close_the_session`."""

    def session(self):
        ours, theirs = socket.socketpair()
        theirs.settimeout(10)
        self.addCleanup(ours.close)
        self.addCleanup(theirs.close)
        peer = Peer(ours, lambda target, method, args: "served")
        self.addCleanup(peer._channel.close)
        self.addCleanup(peer.close)
        return peer, theirs

    async def test_text_that_is_not_json_closes_the_session_and_the_channel(self):
        cases = {
            "malformed JSON": b'{"op":"invoke","id":"rust:1"',
            # json.loads takes bytes that start with a BOM: Rust and Node do not.
            "a leading BOM": b'\xef\xbb\xbf{"op":"cancel","id":"rust:1"}',
            "invalid UTF-8": b'{"op":"cancel","id":"rust:\xff"}',
        }
        for name, text in cases.items():
            with self.subTest(name):
                peer, theirs = self.session()
                peer.start()
                lines = theirs.makefile("rb")
                self.addCleanup(lines.close)
                self.assertEqual(json.loads(lines.readline())["op"], "hello")
                theirs.sendall(text + b"\n")
                await asyncio.wait_for(peer.closed, 10)
                self.assertIsInstance(peer.closed_error, ConnectionError)
                self.assertEqual(lines.readline(), b"", "the channel ends too")

    async def test_malformed_and_dangling_frames_close_the_session(self):
        cases = {
            "not an object": 42,
            "unknown op": {"op": "frobnicate", "id": "rust:1"},
            "wrong field type": {"op": "invoke", "id": "rust:1", "path": [], "target": 7, "method": "m", "args": {"type": "undefined"}},
            "missing field": {"op": "invoke", "id": "rust:1", "path": [], "target": "t", "args": {"type": "undefined"}},
            "unknown wire value": {"op": "invoke", "id": "rust:1", "path": [], "target": "t", "method": "m", "args": {"type": "bogus"}},
            "call of an unknown reference": {"op": "call", "id": "rust:1", "path": [], "reference": 99, "args": {"type": "undefined"}},
            "await of an unknown reference": {"op": "await", "id": "rust:1", "path": [], "reference": 99},
            "release of an unknown reference": {"op": "release", "reference": 99, "count": 1},
            "reply to an unknown call": {"op": "return", "id": "node:99", "value": {"type": "undefined"}},
            "cancel without an id": {"op": "cancel", "id": 7},
        }
        for name, frame in cases.items():
            with self.subTest(name):
                peer, _theirs = self.session()
                peer._receive({"op": "hello", "version": 2})
                peer.receive(frame)
                self.assertIsNotNone(peer.closed_error)
                self.assertTrue(peer.closed.done())

    async def test_a_cancel_for_an_unknown_call_is_ignored(self):
        peer, theirs = self.session()
        peer._receive({"op": "hello", "version": 2})
        lines = theirs.makefile("rb")
        peer.receive({"op": "cancel", "id": "rust:7"})
        peer.receive({"op": "invoke", "id": "rust:1", "path": [], "target": "t", "method": "m", "args": {"type": "data", "value": []}})
        # The call runs on the loop: the reply is read from the far end.
        reply = await asyncio.to_thread(lines.readline)
        self.assertEqual(json.loads(reply)["value"], {"type": "data", "value": "served"})
        self.assertIsNone(peer.closed_error)


class Weather:
    def today(self):
        return "monday"


class CapabilityTests(background.IsolatedAsyncioTestCase):
    """An object reference goes only to a far end that declared `objects`."""

    async def session(self, capabilities):
        ours, theirs = socket.socketpair()
        theirs.settimeout(5)
        self.addCleanup(ours.close)
        self.addCleanup(theirs.close)
        peer = Peer(ours, lambda target, method, args: Weather(), endpoint={"local": "py", "expected": "main"})
        self.addCleanup(peer.close)
        lines = theirs.makefile("rb")
        peer.start()
        self.assertEqual(json.loads(lines.readline())["op"], "hello")
        hello = {"op": "hello", "version": 3, "endpoint": "main", "capabilities": capabilities}
        peer._receive(hello)
        peer._receive({"op": "invoke", "id": "main:1", "path": [], "target": "svc", "method": "get", "args": {"type": "data", "value": []}})
        # The call runs on the loop: let it.
        await asyncio.sleep(0.05)
        return json.loads(lines.readline())

    async def test_an_object_is_refused_to_a_far_end_without_objects(self):
        reply = await self.session(["signals"])
        self.assertEqual(reply["op"], "throw", reply)
        self.assertIn("cannot receive object references", reply["error"]["message"])

    async def test_an_object_goes_to_a_far_end_with_objects(self):
        reply = await self.session(["objects"])
        self.assertEqual(reply["op"], "return", reply)
        self.assertEqual(reply["value"]["type"], "reference")
        self.assertEqual(reply["value"]["value"]["kind"], "object")


class ScriptedChannel:
    """A channel whose far end already sent `incoming` and then ended. It
    notes whether this side had sent anything when it first read; with
    `ended`, sending fails as on a channel the far end closed."""

    def __init__(self, incoming, ended=False):
        self.incoming = list(incoming)
        self.ended = ended
        self.sent = []
        self.sent_before_first_read = None
        self.lock = threading.Lock()

    def send(self, message):
        if self.ended:
            raise BrokenPipeError("the far end closed the channel")
        with self.lock:
            self.sent.append(json.loads(message))

    def recv(self):
        with self.lock:
            if self.sent_before_first_read is None:
                self.sent_before_first_read = bool(self.sent)
        return self.incoming.pop(0) if self.incoming else None

    def close(self, reason=""):
        pass


class HandshakeOrderTests(unittest.IsolatedAsyncioTestCase):
    """#239: a session greets before it reads, so a far end it refuses
    still reads its greeting; and a far end's greeting decides the
    handshake even when the channel ended before this side greeted."""

    async def ready_error(self, peer):
        with self.assertRaises(Exception) as raised:
            # The timeout only stops a hang.
            await asyncio.wait_for(peer.ready, 10)
        return raised.exception

    async def test_issue_239_a_session_greets_before_it_reads_the_far_greeting(self):
        far = json.dumps({"op": "hello", "version": 3, "endpoint": "main"}).encode()
        channel = ScriptedChannel([far])
        peer = Peer(channel, lambda target, method, args: None)
        self.addCleanup(peer.close)
        peer.start()
        error = await self.ready_error(peer)
        self.assertIs(channel.sent_before_first_read, True)
        self.assertEqual(channel.sent[0]["op"], "hello")
        # Incompatible (ValueError), not the channel end (ConnectionError).
        self.assertIsInstance(error, ValueError)
        self.assertNotIsInstance(error, ConnectionError)

    async def test_issue_239_a_greeting_followed_by_the_channel_end_is_incompatible(self):
        far = json.dumps({"op": "hello", "version": 2}).encode()
        channel = ScriptedChannel([far], ended=True)
        peer = Peer(channel, lambda target, method, args: None, endpoint={"local": "py"})
        self.addCleanup(peer.close)
        peer.start()
        error = await self.ready_error(peer)
        self.assertIsInstance(error, ValueError)
        self.assertNotIsInstance(error, ConnectionError)

    async def test_a_channel_end_before_any_greeting_is_a_connection_error(self):
        channel = ScriptedChannel([], ended=True)
        peer = Peer(channel, lambda target, method, args: None, endpoint={"local": "py"})
        self.addCleanup(peer.close)
        peer.start()
        self.assertIsInstance(await self.ready_error(peer), ConnectionError)


if __name__ == "__main__":
    unittest.main()
