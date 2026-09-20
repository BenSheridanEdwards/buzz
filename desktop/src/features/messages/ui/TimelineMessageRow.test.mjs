import assert from "node:assert/strict";
import { test } from "node:test";

import * as React from "react";
import { MessageRow } from "./MessageRow.tsx";
import { MessageRowItem } from "./TimelineMessageRow.tsx";

for (const withThreadSummary of [false, true]) {
  test(`timeline forwards viewer identity to ${withThreadSummary ? "thread-summary" : "plain"} message rows`, () => {
    const viewer = "a".repeat(64);
    const element = MessageRowItem({
      currentPubkey: viewer,
      entry: {
        message: {
          id: "note",
          pubkey: viewer,
          author: "Viewer",
          body: "voice note",
          depth: 0,
          reactions: [],
          tags: [],
        },
        summary: withThreadSummary ? { count: 1 } : undefined,
      },
      footer: null,
      onOpenThread: withThreadSummary ? () => {} : undefined,
    });
    const row = React.Children.toArray(element.props.children).find(
      (child) => React.isValidElement(child) && child.type === MessageRow,
    );
    assert.ok(row, "actual timeline branch must contain MessageRow");
    assert.equal(row.props.currentPubkey, viewer);
    assert.equal(row.props.message.pubkey, viewer);
  });
}
