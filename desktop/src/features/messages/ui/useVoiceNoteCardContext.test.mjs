import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import { JSDOM } from "jsdom";

const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://localhost",
});

before(() => {
  Object.assign(globalThis, {
    document: dom.window.document,
    HTMLElement: dom.window.HTMLElement,
    IS_REACT_ACT_ENVIRONMENT: true,
    window: dom.window,
  });
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: dom.window.navigator,
  });
});

after(() => dom.window.close());

const VOICE_NOTE = new Map([
  [
    "https://relay.example/media/note.wav",
    {
      filename: "voice-note-1.wav",
      m: "audio/wav",
      url: "https://relay.example/media/note.wav",
    },
  ],
]);

function channelList(unreadTick) {
  // A fresh array of fresh objects, the way the channels query hands one out
  // on every channel update (unread counts, names, membership).
  return [
    { id: "channel-1", channelType: "public", name: "general", unreadTick },
    { id: "dm-1", channelType: "dm", name: "alice", unreadTick },
  ];
}

const renderTranscript = () => null;

test("the card identity survives a channel-list update", async () => {
  const { renderHook } = await import("@testing-library/react");
  const { useVoiceNoteCardContext } = await import(
    "./useVoiceNoteCardContext.ts"
  );

  const { result, rerender } = renderHook(
    ({ channels }) =>
      useVoiceNoteCardContext({
        channelId: "channel-1",
        channels,
        imetaByUrl: VOICE_NOTE,
        renderTranscript,
        sender: "Alice",
      }),
    { initialProps: { channels: channelList(0) } },
  );

  const first = result.current;
  assert.equal(first?.conversation, "channel");

  rerender({ channels: channelList(1) });
  assert.equal(
    result.current,
    first,
    "a channel update elsewhere must not give every voice-note row a new card",
  );
});

test("the conversation still follows the channel it belongs to", async () => {
  const { renderHook } = await import("@testing-library/react");
  const { useVoiceNoteCardContext } = await import(
    "./useVoiceNoteCardContext.ts"
  );

  const { result, rerender } = renderHook(
    ({ channelId }) =>
      useVoiceNoteCardContext({
        channelId,
        channels: channelList(0),
        imetaByUrl: VOICE_NOTE,
        renderTranscript,
        sender: "Alice",
      }),
    { initialProps: { channelId: "channel-1" } },
  );
  assert.equal(result.current?.conversation, "channel");

  rerender({ channelId: "dm-1" });
  assert.equal(result.current?.conversation, "dm");

  rerender({ channelId: null });
  assert.equal(
    result.current?.conversation,
    "channel",
    "an unknown conversation falls back to the channel presentation",
  );
});

test("a message with no voice note has no card", async () => {
  const { renderHook } = await import("@testing-library/react");
  const { useVoiceNoteCardContext } = await import(
    "./useVoiceNoteCardContext.ts"
  );

  const { result } = renderHook(() =>
    useVoiceNoteCardContext({
      channelId: "channel-1",
      channels: channelList(0),
      imetaByUrl: new Map([
        [
          "https://relay.example/media/pic.png",
          { filename: "pic.png", m: "image/png" },
        ],
      ]),
      renderTranscript,
      sender: "Alice",
    }),
  );
  assert.equal(result.current, undefined);
});
