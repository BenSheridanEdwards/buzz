"""Isolated GatewayRunner + production ACPListener; Rust is the consumer.
Run with Hermes scripts/run_tests.sh pointing at this absolute file.
Only model construction/resolution and startup provider checks are doubled.
"""
import asyncio
import json
import os
from pathlib import Path
import sys
import tempfile

import pytest

from tests.gateway.test_acp_attach_canonical import runner, ModelDouble


@pytest.fixture(autouse=True)
def isolated_home(tmp_path, monkeypatch):
    monkeypatch.setenv("HERMES_HOME", str(tmp_path))
    monkeypatch.setenv("HERMES_TEST_MODE", "1")


@pytest.mark.asyncio
async def test_rust_canonical_consumer(runner, monkeypatch):
    from gateway.acp_bridge import GatewayACPBridge
    from acp_adapter.local_transport import ACPListener

    bridge = GatewayACPBridge(runner)
    bridge.install_adapter()
    with tempfile.TemporaryDirectory(prefix="buzz-acp-", dir="/tmp") as directory:
        listener = ACPListener(Path(directory), bridge.dispatch)
        await listener.start()
        buzz = Path(__file__).resolve().parents[2]
        env = {**os.environ, "BUZZ_TEST_ACP_SOCKET": str(listener.path),
               "BUZZ_TEST_PYTHON": sys.executable,
               "BUZZ_TEST_HERMES_ROOT": str(Path.cwd()),
               "BUZZ_TEST_STATE": str(Path(directory) / "rust-state")}
        try:
            child = await asyncio.create_subprocess_exec(
                str(buzz / "bin/cargo"), "test", "-p", "buzz-acp", "--lib",
                "rust_gateway_consumer", "--", "--ignored", "--nocapture",
                cwd=buzz, env=env, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT)
            output, _ = await asyncio.wait_for(child.communicate(), 120)
            assert child.returncode == 0, output.decode()
            assert b"1 passed" in output, output.decode()
            assert len(ModelDouble.instances) == 1
            messages = ModelDouble.instances[0]._session_messages
            assert len([m for m in messages if m["role"] == "user"]) == 2
        finally:
            await listener.close()
            if bridge.completion_tasks:
                await asyncio.gather(*bridge.completion_tasks)
