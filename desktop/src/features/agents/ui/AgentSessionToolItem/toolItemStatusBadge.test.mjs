import assert from "node:assert/strict";
import test from "node:test";

import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { ToolItem } from "./ToolItem.tsx";

const timestamp = "2026-06-14T19:00:00.000Z";

function tool(overrides) {
  return {
    id: "tool:1",
    type: "tool",
    title: "Tool call",
    toolName: "shell",
    buzzToolName: null,
    status: "completed",
    args: {},
    result: "",
    isError: false,
    timestamp,
    startedAt: timestamp,
    completedAt: null,
    ...overrides,
  };
}

const branches = {
  inline: { toolName: "shell", args: { command: "ls" } },
  todo: { toolName: "todo", args: { todos: [{ text: "ship", done: false }] } },
};

function render(item) {
  return renderToStaticMarkup(
    React.createElement(ToolItem, {
      agentAvatarUrl: null,
      agentName: "Agent",
      agentPubkey: "aa",
      item,
    }),
  );
}

test("tool status badges are plain text on every ToolItem branch, never per-row live regions", () => {
  for (const [branch, overrides] of Object.entries(branches)) {
    for (const [status, isError, label] of [
      ["pending", false, "Pending"],
      ["executing", false, "Running"],
      ["failed", true, "Failed"],
      ["completed", true, "Failed"],
    ]) {
      const html = render(tool({ ...overrides, status, isError }));
      assert.match(
        html,
        new RegExp(`data-testid="transcript-tool-status"[^>]*>${label}<`),
        `${branch}/${status} shows ${label}`,
      );
      assert.doesNotMatch(html, /role="status"/, `${branch}/${status}`);
    }
    const done = render(tool({ ...overrides, status: "completed" }));
    assert.doesNotMatch(done, /transcript-tool-status/, `${branch}/completed`);
  }
});
