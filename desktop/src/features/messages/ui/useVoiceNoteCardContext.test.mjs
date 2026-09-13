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

const renderTranscript = () => null;

test("the card identity survives an unrelated rerender", async () => {
  const { renderHook } = await import("@testing-library/react");
  const { useVoiceNoteCardContext } = await import(
    "./useVoiceNoteCardContext.ts"
  );

  const { result, rerender } = renderHook(
    ({ tick }) => {
      void tick;
      return useVoiceNoteCardContext({
        imetaByUrl: VOICE_NOTE,
        ownNote: false,
        renderTranscript,
        sender: "Alice",
      });
    },
    { initialProps: { tick: 0 } },
  );

  const first = result.current;
  assert.equal(first?.ownNote, false);
  assert.equal(first?.sender, "Alice");

  rerender({ tick: 1 });
  assert.equal(
    result.current,
    first,
    "a rerender that changes nothing the card shows must keep its identity",
  );
});

test("the card knows when the viewer recorded the note", async () => {
  const { renderHook } = await import("@testing-library/react");
  const { useVoiceNoteCardContext } = await import(
    "./useVoiceNoteCardContext.ts"
  );

  const { result, rerender } = renderHook(
    ({ ownNote }) =>
      useVoiceNoteCardContext({
        imetaByUrl: VOICE_NOTE,
        ownNote,
        renderTranscript,
        sender: "Alice",
      }),
    { initialProps: { ownNote: true } },
  );
  assert.equal(result.current?.ownNote, true);

  rerender({ ownNote: false });
  assert.equal(result.current?.ownNote, false);
});

test("a message without a voice note has no card", async () => {
  const { renderHook } = await import("@testing-library/react");
  const { useVoiceNoteCardContext } = await import(
    "./useVoiceNoteCardContext.ts"
  );

  const { result } = renderHook(() =>
    useVoiceNoteCardContext({
      imetaByUrl: new Map([
        [
          "https://relay.example/media/photo.jpg",
          {
            filename: "photo.jpg",
            m: "image/jpeg",
            url: "https://relay.example/media/photo.jpg",
          },
        ],
      ]),
      ownNote: false,
      renderTranscript,
      sender: "Alice",
    }),
  );
  assert.equal(result.current, undefined);
});
