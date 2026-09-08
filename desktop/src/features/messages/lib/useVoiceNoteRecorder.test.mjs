import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import { JSDOM } from "jsdom";

const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://localhost",
});

class FakeTrack {
  stopped = false;
  stop() {
    this.stopped = true;
  }
}

class FakeStream {
  track = new FakeTrack();
  getTracks() {
    return [this.track];
  }
}

class FakeRecorder extends dom.window.EventTarget {
  static isTypeSupported() {
    return true;
  }
  mimeType = "audio/webm";
  state = "inactive";
  starts = 0;
  start() {
    this.state = "recording";
    this.starts += 1;
  }
  pause() {
    if (this.state === "recording") this.state = "paused";
  }
  resume() {
    if (this.state === "paused") this.state = "recording";
  }
  stop() {
    if (this.state === "inactive") return;
    this.state = "inactive";
    this.dispatchEvent(
      new dom.window.MessageEvent("dataavailable", {
        data: new Blob([new Uint8Array([1])], { type: this.mimeType }),
      }),
    );
    this.dispatchEvent(new dom.window.Event("stop"));
  }
}
const RecorderWithRegistry = new Proxy(FakeRecorder, {
  construct(target, args) {
    const recorder = new target(...args);
    recorders.push(recorder);
    return recorder;
  },
});

const decodeResolvers = [];
const recorders = [];
class FakeAudioContext {
  close() {
    return Promise.resolve();
  }
  createAnalyser() {
    return {
      fftSize: 0,
      smoothingTimeConstant: 0,
      getByteTimeDomainData() {},
    };
  }
  createMediaStreamSource() {
    return { connect() {} };
  }
  decodeAudioData() {
    return new Promise((resolve) => decodeResolvers.push(resolve));
  }
}

const streams = [];
const acquireFakeStream = async () => {
  const stream = new FakeStream();
  streams.push(stream);
  return stream;
};
let getUserMediaImpl = acquireFakeStream;
before(() => {
  Object.assign(globalThis, {
    AudioContext: FakeAudioContext,
    document: dom.window.document,
    DOMException: dom.window.DOMException,
    HTMLElement: dom.window.HTMLElement,
    IS_REACT_ACT_ENVIRONMENT: true,
    MediaRecorder: RecorderWithRegistry,
    window: dom.window,
  });
  Object.defineProperty(dom.window.navigator, "mediaDevices", {
    configurable: true,
    value: {
      getUserMedia: (...args) => getUserMediaImpl(...args),
    },
  });
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: dom.window.navigator,
  });
  dom.window.MediaRecorder = RecorderWithRegistry;
  dom.window.AudioContext = FakeAudioContext;
});

const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

after(() => dom.window.close());

test("permission acquisition is visible, cancellable, and releases a late stream", async () => {
  const { act, cleanup, renderHook } = await import("@testing-library/react");
  const { useVoiceNoteRecorder } = await import("./useVoiceNoteRecorder.ts");
  const { result, unmount } = renderHook(() => useVoiceNoteRecorder());
  const stream = new FakeStream();
  let resolvePermission;
  getUserMediaImpl = () =>
    new Promise((resolve) => {
      resolvePermission = resolve;
    });

  try {
    let startPromise;
    act(() => {
      startPromise = result.current.start();
    });
    assert.equal(result.current.status, "requesting");

    act(() => result.current.cancel());
    assert.equal(result.current.status, "idle");

    await act(async () => {
      resolvePermission(stream);
      await startPromise;
    });
    assert.equal(stream.track.stopped, true);
    assert.equal(result.current.status, "idle");
  } finally {
    getUserMediaImpl = acquireFakeStream;
    unmount();
    cleanup();
  }
});

test("remains usable after Strict Mode replays the mount effect", async () => {
  const { StrictMode, createElement } = await import("react");
  const { act, cleanup, renderHook } = await import("@testing-library/react");
  const { useVoiceNoteRecorder } = await import("./useVoiceNoteRecorder.ts");
  const { result, unmount } = renderHook(() => useVoiceNoteRecorder(), {
    wrapper: ({ children }) => createElement(StrictMode, null, children),
  });

  try {
    await act(() => result.current.start());
    assert.equal(result.current.status, "recording");
    assert.equal(streams.at(-1).track.stopped, false);
  } finally {
    unmount();
    cleanup();
  }
});

test("a cancelled decode cannot stop or attach over a newer recording", async () => {
  const { act, cleanup, renderHook } = await import("@testing-library/react");
  const { useVoiceNoteRecorder } = await import("./useVoiceNoteRecorder.ts");
  const { result, unmount } = renderHook(() => useVoiceNoteRecorder());

  try {
    await act(() => result.current.start());
    let firstFinish;
    await act(async () => {
      firstFinish = result.current.stop();
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    assert.equal(decodeResolvers.length, 1);

    act(() => result.current.cancel());
    assert.equal(await firstFinish, null);
    await act(() => result.current.start());
    const secondTrack = streams.at(-1).track;

    await act(async () => {
      decodeResolvers.shift()({
        duration: 1,
        getChannelData: () => new Float32Array([0]),
        numberOfChannels: 1,
        sampleRate: 8_000,
      });
      await Promise.resolve();
    });

    assert.equal(secondTrack.stopped, false);
    assert.equal(result.current.status, "recording");
  } finally {
    unmount();
    cleanup();
  }
});

test("pause and resume keep one file and freeze the elapsed clock", async () => {
  const { act, cleanup, renderHook } = await import("@testing-library/react");
  const { useVoiceNoteRecorder } = await import("./useVoiceNoteRecorder.ts");
  const { result, unmount } = renderHook(() => useVoiceNoteRecorder());

  try {
    await act(() => result.current.start());
    assert.equal(result.current.status, "recording");
    assert.equal(result.current.locked, false);
    const recorder = recorders.at(-1);

    await act(async () => {
      await wait(220);
    });
    const beforePause = result.current.elapsedSeconds;
    assert.ok(beforePause > 0.1, `clock runs while recording (${beforePause})`);

    act(() => result.current.lock());
    assert.equal(result.current.locked, true);

    act(() => result.current.pause());
    assert.equal(result.current.status, "paused");
    assert.equal(recorder.state, "paused");
    const atPause = result.current.elapsedSeconds;
    await act(async () => {
      await wait(250);
    });
    assert.equal(
      result.current.elapsedSeconds,
      atPause,
      "the clock does not advance while paused",
    );

    act(() => result.current.resume());
    assert.equal(result.current.status, "recording");
    assert.equal(recorder.state, "recording");
    await act(async () => {
      await wait(220);
    });
    const afterResume = result.current.elapsedSeconds;
    assert.ok(afterResume > atPause, "the clock continues after resume");
    assert.ok(
      afterResume < atPause + 0.4,
      `paused time is not counted (${afterResume} vs ${atPause})`,
    );

    let finish;
    await act(async () => {
      finish = result.current.stop();
      await wait(0);
    });
    assert.equal(recorder.starts, 1, "resume never starts a second file");
    await act(async () => {
      decodeResolvers.shift()({
        duration: 1,
        getChannelData: () => new Float32Array([0]),
        numberOfChannels: 1,
        sampleRate: 8_000,
      });
      await Promise.resolve();
    });
    const recording = await finish;
    assert.ok(recording, "a paused-then-resumed session still yields a file");
    assert.equal(recording.file.type, "audio/wav");
    assert.equal(result.current.status, "idle");
    assert.equal(result.current.locked, false);
  } finally {
    unmount();
    cleanup();
  }
});

test("cancel from paused discards and releases the microphone", async () => {
  const { act, cleanup, renderHook } = await import("@testing-library/react");
  const { useVoiceNoteRecorder } = await import("./useVoiceNoteRecorder.ts");
  const { result, unmount } = renderHook(() => useVoiceNoteRecorder());

  try {
    await act(() => result.current.start());
    const track = streams.at(-1).track;
    const recorder = recorders.at(-1);
    act(() => result.current.lock());
    act(() => result.current.pause());
    assert.equal(result.current.status, "paused");

    act(() => result.current.cancel());
    assert.equal(result.current.status, "idle");
    assert.equal(result.current.locked, false);
    assert.equal(result.current.elapsedSeconds, 0);
    assert.equal(track.stopped, true);
    assert.equal(recorder.state, "inactive");
    assert.equal(decodeResolvers.length, 0, "a discarded note is not decoded");
    // Resume after cancel is a no-op rather than a stray restart.
    act(() => result.current.resume());
    assert.equal(result.current.status, "idle");
  } finally {
    unmount();
    cleanup();
  }
});
