import assert from "node:assert/strict";
import test from "node:test";

import {
  claimVoiceNoteRecording,
  discardActiveVoiceNoteRecording,
  getVoiceNoteRecordingOwner,
  subscribeVoiceNoteRecording,
} from "./voiceNoteRecordingRegistry.ts";

test("a second composer cannot claim while the first records", () => {
  const release = claimVoiceNoteRecording("channel", () => {});
  assert.notEqual(release, null);
  assert.equal(getVoiceNoteRecordingOwner(), "channel");

  assert.equal(
    claimVoiceNoteRecording("thread", () => {}),
    null,
    "a docked thread composer must not open a second microphone",
  );

  release?.();
  assert.equal(getVoiceNoteRecordingOwner(), null);
  const threadRelease = claimVoiceNoteRecording("thread", () => {});
  assert.notEqual(threadRelease, null);
  threadRelease?.();
});

test("the same composer may re-claim, and the stale release is inert", () => {
  const first = claimVoiceNoteRecording("channel", () => {});
  const second = claimVoiceNoteRecording("channel", () => {});
  assert.notEqual(second, null);

  first?.();
  assert.equal(
    getVoiceNoteRecordingOwner(),
    "channel",
    "a superseded release must not drop the claim that replaced it",
  );
  second?.();
  assert.equal(getVoiceNoteRecordingOwner(), null);
});

test("discarding reaches the holder from anywhere", () => {
  assert.equal(discardActiveVoiceNoteRecording(), false);

  let discarded = 0;
  const release = claimVoiceNoteRecording("channel", () => {
    discarded += 1;
  });
  assert.equal(discardActiveVoiceNoteRecording(), true);
  assert.equal(discarded, 1);
  release?.();
});

test("subscribers see every claim and release", () => {
  const seen = [];
  const unsubscribe = subscribeVoiceNoteRecording(() => {
    seen.push(getVoiceNoteRecordingOwner());
  });
  const release = claimVoiceNoteRecording("channel", () => {});
  release?.();
  unsubscribe();
  claimVoiceNoteRecording("thread", () => {})?.();
  assert.deepEqual(seen, ["channel", null]);
});
