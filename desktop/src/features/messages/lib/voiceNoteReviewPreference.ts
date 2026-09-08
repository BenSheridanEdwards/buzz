import * as React from "react";

/**
 * Whether a finished voice note waits in the composer for a listen back
 * before it is sent. Off by default: releasing the hold sends straight away.
 * Stored per device; reads are throw-safe (WKWebView can deny storage).
 */
export const VOICE_NOTE_REVIEW_STORAGE_KEY = "buzz.voiceNote.reviewBeforeSend";
export const DEFAULT_VOICE_NOTE_REVIEW_ENABLED = false;

const listeners = new Set<() => void>();
let reviewEnabled = readStoredReviewEnabled();

function readStoredReviewEnabled(): boolean {
  try {
    const stored = globalThis.localStorage?.getItem(
      VOICE_NOTE_REVIEW_STORAGE_KEY,
    );
    if (stored === "on") return true;
    if (stored === "off") return false;
    return DEFAULT_VOICE_NOTE_REVIEW_ENABLED;
  } catch {
    return DEFAULT_VOICE_NOTE_REVIEW_ENABLED;
  }
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function getVoiceNoteReviewEnabled(): boolean {
  return reviewEnabled;
}

export function setVoiceNoteReviewEnabled(enabled: boolean): void {
  reviewEnabled = enabled;
  try {
    globalThis.localStorage?.setItem(
      VOICE_NOTE_REVIEW_STORAGE_KEY,
      enabled ? "on" : "off",
    );
  } catch {
    // Persistence is best-effort; the in-memory preference still applies.
  }
  for (const listener of listeners) listener();
}

export function useVoiceNoteReviewEnabled(): boolean {
  return React.useSyncExternalStore(
    subscribe,
    getVoiceNoteReviewEnabled,
    () => DEFAULT_VOICE_NOTE_REVIEW_ENABLED,
  );
}
