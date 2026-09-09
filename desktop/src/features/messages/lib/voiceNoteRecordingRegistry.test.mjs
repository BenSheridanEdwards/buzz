import assert from "node:assert/strict";
import test from "node:test";

import {
  claimVoiceNoteRecording,
  discardActiveVoiceNoteRecording,
  getVoiceNoteRecordingOwner,
  subscribeVoiceNoteRecording,
} from "./voiceNoteRecordingRegistry.ts";

test("a second composer cannot claim while the first records", () => {
  const release = claimVoiceNoteRecording("channel", () => true);
  assert.notEqual(release, null);
  assert.equal(getVoiceNoteRecordingOwner(), "channel");

  assert.equal(
    claimVoiceNoteRecording("thread", () => true),
    null,
    "a docked thread composer must not open a second microphone",
  );

  release?.();
  assert.equal(getVoiceNoteRecordingOwner(), null);
  const threadRelease = claimVoiceNoteRecording("thread", () => true);
  assert.notEqual(threadRelease, null);
  threadRelease?.();
});

test("the same composer may re-claim, and the stale release is inert", () => {
  const first = claimVoiceNoteRecording("channel", () => true);
  const second = claimVoiceNoteRecording("channel", () => true);
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
    return true;
  });
  assert.equal(discardActiveVoiceNoteRecording(), true);
  assert.equal(discarded, 1);
  release?.();
});

test("a holder that declines is not reported as handled", () => {
  // The holder is the authority: a note that is already encoding refuses the
  // ambient key, and a claim with nothing behind it has nothing to discard.
  // Answering "true" here swallows Escape for every surface in the window.
  const release = claimVoiceNoteRecording("channel", () => false);
  assert.equal(
    discardActiveVoiceNoteRecording(),
    false,
    "a refused discard must leave Escape for whatever is behind the composer",
  );
  release?.();
  assert.equal(discardActiveVoiceNoteRecording(), false);
});

test("subscribers see every claim and release", () => {
  const seen = [];
  const unsubscribe = subscribeVoiceNoteRecording(() => {
    seen.push(getVoiceNoteRecordingOwner());
  });
  const release = claimVoiceNoteRecording("channel", () => true);
  release?.();
  unsubscribe();
  claimVoiceNoteRecording("thread", () => true)?.();
  assert.deepEqual(seen, ["channel", null]);
});
