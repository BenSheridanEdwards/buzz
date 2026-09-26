"""Capture real negotiated Hermes frame/journal/replay producers without a model.
The isolated route lookup is a fixture; production E2E is test_rust_consumer.py.
Run with PYTHONDONTWRITEBYTECODE=1 and PYTHONPATH pointing at read-only Hermes.
"""
import asyncio
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace


async def main():
    with tempfile.TemporaryDirectory(prefix="buzz-v1-wire-") as tmp:
        os.environ["HERMES_HOME"] = tmp
        os.environ["HERMES_TEST_MODE"] = "1"
        from gateway.acp_bridge import GatewayACPBridge
        from gateway.acp_observer import AttachedTurn

        bridge = GatewayACPBridge(SimpleNamespace(config=SimpleNamespace(sessions_dir=Path(tmp))))
        async def entry(sid):
            return SimpleNamespace(session_id=sid, session_key="fixture-route")
        bridge.entry = entry

        class Emit:
            def __init__(self):
                self.frames = []
            def __call__(self, frame):
                self.frames.append(frame)

        emit = Emit()
        initialized = await bridge.initialize({"clientCapabilities": {"_meta": {"hermesAttachment": {"version": 1}}}}, emit)
        turn = AttachedTurn("fixture-session", "fixture-route", emit)
        turn.turn_id = "fixture-turn"
        bridge.turns[turn.session_key] = turn
        turn.done.set_result({"final_response": "Final " + "界" * 30000, "completed": True})
        await bridge.finish_turn(turn, None)
        replay = Emit()
        await bridge.initialize({"clientCapabilities": {"_meta": {"hermesAttachment": {"version": 1}}}}, replay)
        loaded = await bridge.load({"sessionId": turn.session_id, "cwd": "/tmp", "mcpServers": [], "_meta": {"afterDeliveryId": 0, "history": False}}, replay)
        assert loaded["_meta"]["replayComplete"]
        assert any(frame["method"] == "_hermes/turn_complete" for frame in replay.frames)
        output = {"provenance": "Real GatewayACPBridge + AttachedTurn + DeliveryJournal producers; isolated route lookup, no hand-authored protocol frames", "initialize": initialized, "frames": replay.frames, "load": loaded}
        path = Path(__file__).with_name("canonical-v1.json")
        path.write_text(json.dumps(output, ensure_ascii=False, indent=2) + "\n")
        print(path, "frames:", len(replay.frames))


if __name__ == "__main__":
    asyncio.run(main())
