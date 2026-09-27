import assert from "node:assert/strict";
import test from "node:test";
import {
  buildTranscript,
  createEmptyTranscriptState,
  processTranscriptEvent,
} from "./agentSessionTranscript.ts";

function event(seq, update) {
  return {
    seq,
    timestamp: `2026-06-18T00:00:0${seq}Z`,
    kind: "acp_read",
    agentIndex: 0,
    channelId: "channel",
    sessionId: "session",
    turnId: "turn",
    payload: {
      method: "session/update",
      params: { update: { toolCallId: "tool", ...update } },
    },
  };
}

test("statusless tool progress preserves prior status and omitted fields in live and replay paths", () => {
  for (const status of ["pending", "in_progress", "completed", "failed"]) {
    const start = event(1, {
      sessionUpdate: "tool_call",
      title: "Read file",
      rawInput: { path: "a.ts" },
      rawOutput: "first output",
      status,
    });
    const progress = event(2, {
      sessionUpdate: "tool_call_update",
      rawOutput: "partial output",
    });
    const omitted = event(3, { sessionUpdate: "tool_call_update" });
    let state = processTranscriptEvent(createEmptyTranscriptState(), start);
    const original = state.items[0];
    state = processTranscriptEvent(state, progress);
    state = processTranscriptEvent(state, omitted);
    const tool = state.items[0];
    assert.equal(tool.status, original.status, status);
    assert.equal(tool.completedAt, original.completedAt);
    assert.equal(tool.title, original.title);
    assert.equal(tool.toolName, original.toolName);
    assert.deepEqual(tool.args, original.args);
    assert.equal(tool.result, "partial output");
    assert.equal(tool.isError, original.isError);
    assert.deepEqual(buildTranscript([start, progress, omitted]), state.items);
  }
});

test("orphan progress stays executing until explicit completion and never reopens", () => {
  const progress = event(1, {
    sessionUpdate: "tool_call_update",
    rawOutput: "partial",
  });
  assert.equal(buildTranscript([progress])[0].status, "executing");
  assert.equal(buildTranscript([progress])[0].completedAt, null);
  for (const terminal of ["completed", "failed"]) {
    const done = event(2, {
      sessionUpdate: "tool_call_update",
      status: terminal,
    });
    for (const lateStatus of [undefined, "pending", "in_progress"]) {
      const late = event(3, {
        sessionUpdate: "tool_call_update",
        status: lateStatus,
      });
      const tool = buildTranscript([progress, done, late])[0];
      assert.equal(tool.status, terminal);
      assert.equal(tool.completedAt, done.timestamp);
      assert.equal(tool.result, "partial");
      assert.equal(tool.isError, terminal === "failed");
    }
  }
});
