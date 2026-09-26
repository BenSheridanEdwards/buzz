"""Capture Hermes wire producers without a model, gateway process or live profile.

Usage: python generate.py /absolute/path/to/hermes-checkout > wire.json
The store double supplies history; GatewayACPBridge and AttachedTurn emit all
protocol payloads, and DeliveryJournal uses an isolated real SQLite database.
"""
import asyncio
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(sys.argv[1]).resolve()))


async def capture(directory):
    from gateway.acp_bridge import GatewayACPBridge
    from gateway.acp_observer import AttachedTurn

    async def messages(*args, **kwargs):
        return [{"role": "assistant", "content": "old reply"}]

    runner = SimpleNamespace(
        config=SimpleNamespace(sessions_dir=Path(directory)),
        _session_db=SimpleNamespace(get_messages=messages),
    )
    bridge = GatewayACPBridge(runner)

    async def entry(session_id):
        return SimpleNamespace(session_id=session_id, session_key="route")

    bridge.entry = entry
    history = []
    idle = await bridge.load({"sessionId": "s"}, history.append)
    turn = AttachedTurn("s", "route", lambda message: None)
    bridge.turns["route"] = turn
    turn.text("live reply")
    turn.tool_start("call-1", "terminal", {"command": "printf test"})
    await asyncio.sleep(0)
    active_updates = []
    active = await bridge.load({"sessionId": "s"}, active_updates.append)
    active_snapshot = list(active_updates)
    turn.done.set_result({"final_response": "live reply", "completed": True})
    prompt_result = await bridge.finish_turn(turn, None)
    journal_updates = []
    journal = await bridge.load({"sessionId": "s"}, journal_updates.append)
    return {
        "initialize": await bridge.initialize({}, lambda _: None),
        "history": history,
        "idleLoad": idle,
        "activeLoad": active,
        "activeUpdates": active_snapshot,
        "promptResult": prompt_result,
        "journalLoad": journal,
        "journalUpdates": journal_updates,
    }


with tempfile.TemporaryDirectory(prefix="buzz-hermes-wire-") as directory:
    os.environ["HERMES_HOME"] = directory
    print(json.dumps(asyncio.run(capture(directory)), indent=2))
