import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { waitForAnimations } from "../helpers/animations";
import { installMockBridge } from "../helpers/bridge";
import { expectSmoothCorners } from "../helpers/css";

const AUDIO_URL = "http://127.0.0.1:4173/sounds/ping.mp3";
const IMAGE_URL = "http://127.0.0.1:4173/app-icon@2x.png";

async function openMoreActionsMenu(page: Page, messageId: string) {
  const row = page.locator(`[data-message-id="${messageId}"]`);
  await row.hover();
  await page.getByTestId(`more-actions-${messageId}`).click();
  await expect(page.locator('[role="menuitem"]').first()).toBeVisible();
}

/**
 * Start a hands-free recording from the keyboard: Enter on the focused mic is
 * the click-activation path (no pointer hold to release), so the note waits
 * for an explicit Send. Pointer holds live in voice-note-recorder.spec.ts.
 */
async function startLockedRecording(page: Page) {
  const mic = page.getByRole("button", { name: "Record voice note" });
  await mic.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("voice-note-recorder")).toBeVisible();
}

/**
 * A received voice note. The transcript travels as the attachment's imeta
 * `alt` (what the sender or its transcriber heard); `caption` is ordinary
 * message prose alongside the link and stays in the body.
 */
async function emitVoiceNoteMessage(
  page: Page,
  channelName: string,
  audioUrl: string,
  transcript?: string,
  { caption, imageUrl }: { caption?: string; imageUrl?: string } = {},
) {
  await page.evaluate(
    ({ audioUrl, caption, channelName, imageUrl, transcript }) => {
      const emit = window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__;
      if (!emit) throw new Error("Mock message emitter is unavailable.");
      const lines = [
        caption,
        imageUrl ? `![pic](${imageUrl})` : undefined,
        `[voice-note-123.mp4](${audioUrl})`,
      ].filter((line): line is string => line !== undefined);
      emit({
        channelName,
        content: lines.join("\n"),
        extraTags: [
          [
            "imeta",
            `url ${audioUrl}`,
            "m video/mp4",
            "duration 9.4",
            "filename voice-note-123.mp4",
            ...(transcript ? [`alt ${transcript}`] : []),
          ],
          ...(imageUrl
            ? [
                [
                  "imeta",
                  `url ${imageUrl}`,
                  "m image/png",
                  "dim 128x128",
                  "filename pic.png",
                ],
              ]
            : []),
        ],
      });
    },
    { audioUrl, caption, channelName, imageUrl, transcript },
  );
}

async function waitForMockLiveSubscription(page: Page, channelName: string) {
  await expect
    .poll(() =>
      page.evaluate(
        (currentChannelName) =>
          (
            window as Window & {
              __BUZZ_E2E_HAS_MOCK_LIVE_SUBSCRIPTION__?: (input: {
                channelName: string;
              }) => boolean;
            }
          ).__BUZZ_E2E_HAS_MOCK_LIVE_SUBSCRIPTION__?.({
            channelName: currentChannelName,
          }) ?? false,
        channelName,
      ),
    )
    .toBe(true);
}

test.beforeEach(async ({ page }, testInfo) => {
  // These specs exercise the review flow (finished note waits in the composer
  // for Send). Release-sends coverage lives in voice-note-recorder.spec.ts.
  await page.addInitScript(() => {
    window.localStorage.setItem("buzz.voiceNote.reviewBeforeSend", "on");
  });
  await page.addInitScript(() => {
    Object.defineProperty(navigator.mediaDevices, "getUserMedia", {
      configurable: true,
      async value() {
        const context = new AudioContext();
        const oscillator = context.createOscillator();
        const destination = context.createMediaStreamDestination();
        oscillator.frequency.value = 220;
        oscillator.connect(destination);
        oscillator.start();
        destination.stream.getAudioTracks()[0]?.addEventListener(
          "ended",
          () => {
            oscillator.stop();
            void context.close();
          },
          { once: true },
        );
        return destination.stream;
      },
    });
  });

  await installMockBridge(page, {
    deferredComposerUploads: true,
    sendMessageErrors: testInfo.title.includes("shows a send error")
      ? ["relay rejected voice note"]
      : undefined,
    uploadDescriptors: [
      {
        duration: 9.4,
        filename: "voice-note-123.mp4",
        sha256: "a".repeat(64),
        size: 16424,
        type: "video/mp4",
        uploaded: Math.floor(Date.now() / 1000),
        url: AUDIO_URL,
      },
    ],
  });
});

test("restores the voice note and shows a send error after upload succeeds", async ({
  page,
}) => {
  await page.goto("/");
  const channel = page.getByTestId("channel-general");
  await expect(channel).toBeVisible({ timeout: 10_000 });
  await channel.click();

  await startLockedRecording(page);
  await page.waitForTimeout(100);
  await page.getByRole("button", { name: "Send voice note" }).click();

  const composerCard = page.getByTestId("composer-voice-note-card");
  await expect(composerCard).toBeVisible();
  await page.getByTestId("send-message").click();

  await expect(composerCard).toBeVisible();
  await expect(
    page.getByText("Message failed to send: relay rejected voice note"),
  ).toBeVisible();
});

test("keeps pasted snapshots and channel drops out of an active voice note", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("channel-general").click();

  await startLockedRecording(page);
  await page.waitForTimeout(100);

  await page.getByRole("button", { name: "Send voice note" }).click();
  await expect(page.getByTestId("composer-voice-note-card")).toBeVisible();

  await page
    .getByTestId("message-composer")
    .locator(".ProseMirror")
    .evaluate((editor) => {
      const payload = encodeURIComponent(
        JSON.stringify({
          version: 1,
          displayName: "Snapshot",
          filename: "shared.agent.png",
          sha256: "b".repeat(64),
          size: 128,
          type: "image/png",
          url: "https://relay.example/media/shared.agent.png",
        }),
      );
      const clipboardData = new DataTransfer();
      clipboardData.setData(
        "text/html",
        `<a data-buzz-agent-snapshot="${payload}" href="https://relay.example/media/shared.agent.png">Snapshot</a>`,
      );
      editor.dispatchEvent(
        new ClipboardEvent("paste", {
          bubbles: true,
          cancelable: true,
          clipboardData,
        }),
      );
    });
  await expect(page.getByTestId("composer-agent-snapshot-card")).toHaveCount(0);

  const dataTransfer = await page.evaluateHandle(() => {
    const transfer = new DataTransfer();
    transfer.items.add(
      new File(["second attachment"], "second-attachment.pdf", {
        type: "application/pdf",
      }),
    );
    return transfer;
  });
  const dropZone = page.getByTestId("channel-drop-zone");
  await dropZone.dispatchEvent("dragenter", { dataTransfer });
  await expect(dropZone.getByTestId("drop-zone-overlay")).toHaveCount(0);
  await dropZone.dispatchEvent("drop", { dataTransfer });
  await expect(page.getByTestId("message-composer")).not.toContainText(
    "second-attachment.pdf",
  );
  await expect(page.getByTestId("composer-voice-note-card")).toBeVisible();
});

test("discards an active recording when entering edit mode", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("channel-general").click();

  await startLockedRecording(page);

  await openMoreActionsMenu(page, "mock-general-welcome");
  await page.getByTestId("edit-message-mock-general-welcome").click();

  await expect(page.getByTestId("edit-target")).toBeVisible();
  await expect(page.getByTestId("voice-note-recorder")).toHaveCount(0);
  await expect(page.getByTestId("composer-voice-note-card")).toHaveCount(0);
});

test("editor Enter never saves an edit while a voice note is recording", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("channel-general").click();

  // Enter edit mode first, then start recording — the mic is available in edit
  // mode, so this is the ordering the recorder submission gate must cover.
  await openMoreActionsMenu(page, "mock-general-welcome");
  await page.getByTestId("edit-message-mock-general-welcome").click();
  await expect(page.getByTestId("edit-target")).toBeVisible();
  const input = page.getByTestId("message-input");
  await expect(input).not.toBeEmpty();

  // Change the text so a save (if it wrongly happened) is observable, then
  // start recording.
  const editedContent = `Edited mid-recording ${Date.now()}`;
  await input.click();
  await page.keyboard.press("ControlOrMeta+A");
  await page.keyboard.type(editedContent);

  await startLockedRecording(page);
  await page.waitForTimeout(100);

  // The editor's Enter shortcut must not slip past the Send/Discard flow: the
  // edit stays unsaved and the recording stays live until explicitly resolved.
  await input.focus();
  await input.press("Enter");

  await expect(page.getByTestId("edit-target")).toBeVisible();
  await expect(page.getByTestId("voice-note-recorder")).toBeVisible();
  await expect(page.getByTestId("message-timeline")).not.toContainText(
    editedContent,
  );

  // Sending the recording still works, proving the note was never discarded.
  await page.getByRole("button", { name: "Send voice note" }).click();
  await expect(page.getByTestId("composer-voice-note-card")).toBeVisible();
});

test("discards while voice-note processing is pending", async ({ page }) => {
  await page.addInitScript(() => {
    const pendingDecodes: Array<(buffer: AudioBuffer) => void> = [];
    (
      window as Window & {
        __BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__?: () => void;
      }
    ).__BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__ = () => {
      pendingDecodes.shift()?.({
        duration: 1,
        getChannelData: () => new Float32Array([0]),
        numberOfChannels: 1,
        sampleRate: 8_000,
      } as AudioBuffer);
    };
    AudioContext.prototype.decodeAudioData = () =>
      new Promise<AudioBuffer>((resolve) => pendingDecodes.push(resolve));
  });
  await page.goto("/");
  await page.getByTestId("channel-general").click();

  const discardPendingRecording = async (keyboard: boolean) => {
    await startLockedRecording(page);
    await page.waitForTimeout(100);
    await page.getByRole("button", { name: "Send voice note" }).click();
    await expect(page.getByText("Preparing voice note…")).toBeVisible();
    const discard = page.getByRole("button", { name: "Discard voice note" });
    await expect(discard).toBeEnabled();
    if (keyboard) {
      await discard.focus();
      await page.keyboard.press("Enter");
    } else {
      await discard.click();
    }
    await expect(page.getByTestId("voice-note-recorder")).toHaveCount(0);
    await page.evaluate(() =>
      (
        window as Window & {
          __BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__?: () => void;
        }
      ).__BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__?.(),
    );
    await expect(page.getByTestId("composer-voice-note-card")).toHaveCount(0);
  };

  await discardPendingRecording(false);
  await discardPendingRecording(true);
});

test("surfaces waveform and playback failures with retry", async ({ page }) => {
  await page.addInitScript(() => {
    AudioContext.prototype.decodeAudioData = () =>
      Promise.reject(new DOMException("corrupt audio", "EncodingError"));
    HTMLMediaElement.prototype.play = () =>
      Promise.reject(new DOMException("playback denied", "NotAllowedError"));
  });
  await page.goto("/");
  await page.getByTestId("channel-general").click();
  await waitForMockLiveSubscription(page, "general");
  await page.evaluate(
    ({ audioUrl }) => {
      const emit = (
        window as Window & {
          __BUZZ_E2E_EMIT_MOCK_MESSAGE__?: (input: {
            channelName: string;
            content: string;
            extraTags: string[][];
          }) => unknown;
        }
      ).__BUZZ_E2E_EMIT_MOCK_MESSAGE__;
      if (!emit) throw new Error("Mock message emitter is unavailable.");
      emit({
        channelName: "general",
        content: `[voice-note-123.mp4](${audioUrl})`,
        extraTags: [
          [
            "imeta",
            `url ${audioUrl}`,
            "m video/mp4",
            "duration 9.4",
            "filename voice-note-123.mp4",
          ],
        ],
      });
    },
    { audioUrl: AUDIO_URL },
  );

  const card = page.getByTestId("audio-message-attachment").last();
  const waveform = card.getByTestId("voice-note-playback-waveform");
  await expect(waveform).toHaveAttribute("data-waveform-state", "error");
  await expect(card.getByRole("status")).toContainText(
    "Waveform preview unavailable. Playback may still work.",
  );

  await card.getByRole("button", { name: "Play voice note" }).click();
  await expect(card.getByRole("alert")).toContainText("Audio unavailable");
  const retry = card.getByRole("button", { name: "Retry voice note" });
  await retry.click();
  await expect(
    card.getByRole("button", { name: "Play voice note" }),
  ).toBeVisible();
  await card.locator("audio").dispatchEvent("error");
  await expect(card.getByRole("alert")).toContainText("Audio unavailable");
});

test("resumes a Play click that landed before the source finished loading", async ({
  page,
}) => {
  let releaseFetch: (() => void) | undefined;
  const fetchGate = new Promise<void>((resolve) => {
    releaseFetch = resolve;
  });
  // Gate the media fetch so the received card has no playback source when the
  // user first clicks Play.
  await page.route(AUDIO_URL, async (route) => {
    await fetchGate;
    await route.continue();
  });

  await page.goto("/");
  await page.getByTestId("channel-general").click();
  await waitForMockLiveSubscription(page, "general");
  await page.evaluate(
    ({ audioUrl }) => {
      const emit = (
        window as Window & {
          __BUZZ_E2E_EMIT_MOCK_MESSAGE__?: (input: {
            channelName: string;
            content: string;
            extraTags: string[][];
          }) => unknown;
        }
      ).__BUZZ_E2E_EMIT_MOCK_MESSAGE__;
      if (!emit) throw new Error("Mock message emitter is unavailable.");
      emit({
        channelName: "general",
        content: `[voice-note-123.mp4](${audioUrl})`,
        extraTags: [
          [
            "imeta",
            `url ${audioUrl}`,
            "m video/mp4",
            "duration 9.4",
            "filename voice-note-123.mp4",
          ],
        ],
      });
    },
    { audioUrl: AUDIO_URL },
  );

  const card = page.getByTestId("audio-message-attachment").last();
  await expect(card).toBeVisible();

  // Click Play while the fetch is still held: the intent must be remembered and
  // surfaced as a loading state, not silently dropped.
  await card.getByRole("button", { name: "Play voice note" }).click();
  await expect(
    card.getByRole("button", { name: "Loading voice note" }),
  ).toBeVisible();

  // Releasing the fetch resolves the source; playback starts without a second
  // click.
  releaseFetch?.();
  await expect(
    card.getByRole("button", { name: "Pause voice note" }),
  ).toBeVisible();
});

test("generic audio retains the relay-native download action", async ({
  page,
}) => {
  const audioUrl = `http://localhost:3000/media/${"d".repeat(64)}.mp3`;
  await page.goto("/");
  await page.getByTestId("channel-general").click();
  await waitForMockLiveSubscription(page, "general");
  await page.evaluate(
    ({ href }) => {
      const emit = window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__;
      if (!emit) throw new Error("Mock message emitter is unavailable.");
      emit({
        channelName: "general",
        content: `[meeting.mp3](${href})`,
        extraTags: [
          ["imeta", `url ${href}`, "m audio/mpeg", "filename meeting.mp3"],
        ],
      });
    },
    { href: audioUrl },
  );

  const card = page.getByTestId("audio-message-attachment").last();
  const download = card.getByRole("button", { name: "Download meeting.mp3" });
  await expect(download).toBeVisible();
  await download.click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          window.__BUZZ_E2E_COMMAND_LOG__?.find(
            ({ command }) => command === "download_file",
          ) ?? null,
      ),
    )
    .toEqual({
      command: "download_file",
      payload: { filename: "meeting.mp3", url: audioUrl },
    });
});

test("voice notes omit the relay-native download action", async ({ page }) => {
  const audioUrl = `http://localhost:3000/media/${"e".repeat(64)}.mp4`;
  await page.goto("/");
  await page.getByTestId("channel-general").click();
  await waitForMockLiveSubscription(page, "general");
  await page.evaluate(
    ({ href }) => {
      const emit = window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__;
      if (!emit) throw new Error("Mock message emitter is unavailable.");
      emit({
        channelName: "general",
        content: `[voice-note-123.mp4](${href})`,
        extraTags: [
          [
            "imeta",
            `url ${href}`,
            "m video/mp4",
            "duration 9.4",
            "filename voice-note-123.mp4",
          ],
        ],
      });
    },
    { href: audioUrl },
  );

  const card = page.getByTestId("audio-message-attachment").last();
  await expect(card).toBeVisible();
  await expect(
    card.getByRole("button", { name: "Download voice-note-123.mp4" }),
  ).toHaveCount(0);
});

test("hard-caps audio work and cancels active and queued loads on unmount", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.__BUZZ_E2E_HOLD_MEDIA_FETCHES__ = true;
    const originalCreate = URL.createObjectURL.bind(URL);
    const originalRevoke = URL.revokeObjectURL.bind(URL);
    const counters = { created: 0, revoked: 0 };
    (
      window as Window & {
        __BUZZ_E2E_AUDIO_OBJECT_URLS__?: typeof counters;
      }
    ).__BUZZ_E2E_AUDIO_OBJECT_URLS__ = counters;
    URL.createObjectURL = (object) => {
      counters.created += 1;
      return originalCreate(object);
    };
    URL.revokeObjectURL = (url) => {
      counters.revoked += 1;
      originalRevoke(url);
    };
  });
  await page.setViewportSize({ width: 1280, height: 400 });
  await page.goto("/");
  await page.getByTestId("channel-general").click();
  await waitForMockLiveSubscription(page, "general");
  await page.evaluate(
    ({ audioUrl }) => {
      const emit = (
        window as Window & {
          __BUZZ_E2E_EMIT_MOCK_MESSAGE__?: (input: {
            channelName: string;
            content: string;
            extraTags: string[][];
          }) => unknown;
        }
      ).__BUZZ_E2E_EMIT_MOCK_MESSAGE__;
      if (!emit) throw new Error("Mock message emitter is unavailable.");
      for (let index = 0; index < 24; index += 1) {
        emit({
          channelName: "general",
          content: `[voice-note-${index}.mp4](${audioUrl})`,
          extraTags: [
            [
              "imeta",
              `url ${audioUrl}`,
              "m video/mp4",
              "duration 9.4",
              `filename voice-note-${index}.mp4`,
            ],
          ],
        });
      }
    },
    { audioUrl: AUDIO_URL },
  );

  const cards = page.getByTestId("audio-message-attachment");
  await expect(cards).toHaveCount(24);
  const readFetchCount = () =>
    page.evaluate(
      () =>
        window.__BUZZ_E2E_COMMANDS__?.filter(
          (command) => command === "fetch_media_bytes",
        ).length ?? 0,
    );
  await expect
    .poll(() =>
      page.evaluate(
        () => window.__BUZZ_E2E_MEDIA_FETCH_STATE__ ?? { active: 0, peak: 0 },
      ),
    )
    .toEqual({ active: 3, peak: 3 });
  expect(await readFetchCount()).toBe(3);
  expect(
    await page.evaluate(
      () => window.__BUZZ_E2E_AUDIO_OBJECT_URLS__?.created ?? 0,
    ),
  ).toBe(0);

  await page.getByTestId("channel-random").click();
  await expect(cards).toHaveCount(0);
  await expect
    .poll(() =>
      page.evaluate(() => window.__BUZZ_E2E_MEDIA_FETCH_STATE__?.active ?? -1),
    )
    .toBe(0);
  const commandCounts = await page.evaluate(() => {
    const commands = window.__BUZZ_E2E_COMMANDS__ ?? [];
    return {
      cancelled: commands.filter((command) => command === "cancel_media_fetch")
        .length,
      fetched: commands.filter((command) => command === "fetch_media_bytes")
        .length,
      released: commands.filter((command) => command === "release_media_fetch")
        .length,
    };
  });
  expect(commandCounts).toEqual({ cancelled: 3, fetched: 3, released: 3 });
  expect(
    await page.evaluate(() => window.__BUZZ_E2E_AUDIO_OBJECT_URLS__),
  ).toEqual({ created: 0, revoked: 0 });
});

test("records from the composer and renders an inline waveform card", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.setViewportSize({ width: 2200, height: 720 });
  await page.goto("/");
  const showError = page.getByRole("button", { name: "Show Error" });
  const channel = page.getByTestId("channel-general");
  await expect(channel.or(showError)).toBeVisible({ timeout: 10_000 });
  if (await showError.isVisible()) {
    await showError.click();
    throw new Error(await page.locator("body").innerText());
  }
  await channel.click();

  const attach = page.getByRole("button", { name: "Attach file" });
  const record = page.getByRole("button", { name: "Record voice note" });
  const emoji = page.getByRole("button", { name: "Emoji" });
  await expect(record).toBeVisible();

  const [attachBox, recordBox, emojiBox] = await Promise.all([
    attach.boundingBox(),
    record.boundingBox(),
    emoji.boundingBox(),
  ]);
  expect(attachBox).not.toBeNull();
  expect(recordBox).not.toBeNull();
  expect(emojiBox).not.toBeNull();
  expect(recordBox?.x).toBeGreaterThan(attachBox?.x ?? 0);
  expect(recordBox?.x).toBeLessThan(emojiBox?.x ?? Number.POSITIVE_INFINITY);

  await startLockedRecording(page);
  const recorder = page.getByTestId("voice-note-recorder");
  await expect(recorder).toBeVisible();
  // Keyboard activation has no release to wait for, so it starts locked.
  await expect(recorder).toHaveAttribute("data-voice-note-state", "locked");
  await expect(page.getByTestId("voice-note-lock-chip")).toHaveAttribute(
    "data-locked",
    "true",
  );
  // One polite live region announces transitions; the ticking timer is
  // not read out.
  await expect(page.getByTestId("voice-note-live-status")).toHaveText(
    "Recording voice note, hands free",
  );
  await expect(page.getByTestId("voice-note-live-status")).toHaveAttribute(
    "aria-live",
    "polite",
  );
  await expect(attach).toBeHidden();
  const discard = page.getByRole("button", { name: "Discard voice note" });
  await expect(discard).toBeVisible();
  const [recorderBox, discardBox] = await Promise.all([
    recorder.boundingBox(),
    discard.boundingBox(),
  ]);
  expect(recorderBox).not.toBeNull();
  expect(discardBox).not.toBeNull();
  expect(discardBox?.x ?? Number.POSITIVE_INFINITY).toBeLessThanOrEqual(
    (recorderBox?.x ?? 0) + 1,
  );
  await expect(recorder).toContainText(/\d+:\d{2} \/ 5:00/);
  const finish = page.getByRole("button", { name: "Send voice note" });
  await expect(finish).toBeEnabled();
  await expect(finish).toHaveAttribute("data-voice-note-locked", "true");
  await expect(finish).toHaveText("Send");
  const liveWaveform = page.getByTestId("voice-note-live-waveform");
  const firstRecordedSample = liveWaveform.locator(
    '[data-waveform-sample="recorded-0"]',
  );
  await expect(firstRecordedSample).toBeAttached();
  const firstSampleStart = await firstRecordedSample.boundingBox();
  await page.waitForTimeout(200);
  const firstSampleAfter = await firstRecordedSample.boundingBox();
  expect(firstSampleStart).not.toBeNull();
  expect(firstSampleAfter).not.toBeNull();
  expect(firstSampleAfter?.x ?? 0).toBeLessThan((firstSampleStart?.x ?? 0) - 2);
  expect(firstSampleAfter?.height).toBe(firstSampleStart?.height);
  const [waveformBox, lastBarBox] = await Promise.all([
    liveWaveform.boundingBox(),
    liveWaveform.locator("span").last().boundingBox(),
  ]);
  expect(waveformBox).not.toBeNull();
  expect(lastBarBox).not.toBeNull();
  expect((lastBarBox?.x ?? 0) + (lastBarBox?.width ?? 0)).toBeGreaterThan(
    (waveformBox?.x ?? 0) + (waveformBox?.width ?? 0) - 6,
  );
  await waitForAnimations(page);
  await page.getByTestId("message-composer-toolbar").screenshot({
    path: "test-results/voice-note/voice-note-recorder.png",
  });

  await finish.click();
  const composerCard = page.getByTestId("composer-voice-note-card");
  await expect(composerCard).toBeVisible();
  await waitForAnimations(page);
  // The cap is the point: 21rem in a 2200px composer. Assert the cap itself,
  // then let the rendered box settle onto it. The card enters on a spring
  // that overshoots the resting scale by a fraction of a percent, so a single
  // zero-tolerance read of the animated box is a race with the last frame
  // (336.17 idle here, 338.80 under load).
  await expect(composerCard).toHaveCSS("max-width", "336px");
  await expect
    .poll(
      async () =>
        (await composerCard.boundingBox())?.width ?? Number.POSITIVE_INFINITY,
      {
        message: "the composer voice-note card settles at its 21rem cap",
        timeout: 10_000,
      },
    )
    .toBeLessThanOrEqual(336);
  const composerCardBox = await composerCard.boundingBox();
  expect(composerCardBox).not.toBeNull();
  await expect(
    composerCard.getByRole("button", { name: "Play voice note" }),
  ).toBeVisible();
  await expect(
    composerCard.getByRole("slider", { name: "Voice note playback position" }),
  ).toBeVisible();
  await expect(composerCard).toContainText(/\d+:\d{2}/);
  await expect(composerCard).not.toContainText(/· Voice note/);
  const composerVoiceNote = composerCard.locator("..");
  const removeVoiceNote = composerVoiceNote.getByTestId(
    "remove-composer-voice-note",
  );
  await expect(removeVoiceNote).toHaveCSS("opacity", "0");
  await composerVoiceNote.hover();
  await expect(removeVoiceNote).toHaveCSS("opacity", "1");
  const removeVoiceNoteBox = await removeVoiceNote.boundingBox();
  expect(removeVoiceNoteBox).not.toBeNull();
  expect(removeVoiceNoteBox?.x ?? Number.POSITIVE_INFINITY).toBeLessThan(
    (composerCardBox?.x ?? 0) + (composerCardBox?.width ?? 0),
  );
  expect(removeVoiceNoteBox?.y ?? Number.POSITIVE_INFINITY).toBeLessThan(
    composerCardBox?.y ?? 0,
  );
  expect(
    (removeVoiceNoteBox?.x ?? 0) + (removeVoiceNoteBox?.width ?? 0),
  ).toBeGreaterThan((composerCardBox?.x ?? 0) + (composerCardBox?.width ?? 0));
  expect(
    (removeVoiceNoteBox?.y ?? 0) + (removeVoiceNoteBox?.height ?? 0),
  ).toBeGreaterThan(composerCardBox?.y ?? 0);
  const composerPlaybackWaveform = composerCard.getByTestId(
    "voice-note-playback-waveform",
  );
  const [playbackBox, playbackLastBarBox] = await Promise.all([
    composerPlaybackWaveform.boundingBox(),
    composerPlaybackWaveform.locator("span").last().boundingBox(),
  ]);
  expect(playbackBox).not.toBeNull();
  expect(playbackLastBarBox).not.toBeNull();
  expect(
    (playbackLastBarBox?.x ?? 0) + (playbackLastBarBox?.width ?? 0),
  ).toBeGreaterThan((playbackBox?.x ?? 0) + (playbackBox?.width ?? 0) - 6);
  await waitForAnimations(page);
  await page.screenshot({
    clip: {
      height: (composerCardBox?.height ?? 0) + 16,
      width: (composerCardBox?.width ?? 0) + 16,
      x: (composerCardBox?.x ?? 0) - 8,
      y: (composerCardBox?.y ?? 0) - 8,
    },
    path: "test-results/voice-note/voice-note-composer-card.png",
  });
  await expect(page.getByTestId("send-message")).toBeEnabled();
  await page.getByTestId("send-message").click();

  const card = page.getByTestId("audio-message-attachment").last();
  await expect(card).toBeVisible();
  const playbackWaveform = card.getByTestId("voice-note-playback-waveform");
  await expect(playbackWaveform).toHaveAttribute(
    "data-waveform-state",
    "ready",
  );
  await expect
    .poll(() =>
      playbackWaveform
        .locator("span")
        .evaluateAll((bars) =>
          bars.some((bar) => bar.getBoundingClientRect().height > 3),
        ),
    )
    .toBe(true);
  const waveformGeometry = await playbackWaveform.evaluate((waveform) => {
    const waveformBox = waveform.getBoundingClientRect();
    return Array.from(waveform.querySelectorAll("span")).map((bar) => {
      const barBox = bar.getBoundingClientRect();
      return {
        bottom: barBox.bottom,
        radius: Number.parseFloat(getComputedStyle(bar).borderTopLeftRadius),
        top: barBox.top,
        waveformBottom: waveformBox.bottom,
        waveformTop: waveformBox.top,
        width: barBox.width,
      };
    });
  });
  expect(waveformGeometry.length).toBeGreaterThan(0);
  for (const bar of waveformGeometry) {
    expect(bar.radius).toBeGreaterThanOrEqual(bar.width / 2);
    expect(bar.top).toBeGreaterThanOrEqual(bar.waveformTop);
    expect(bar.bottom).toBeLessThanOrEqual(bar.waveformBottom);
  }
  const playbackControl = card.getByTestId("voice-note-playback-control");
  await expect(card).toHaveClass(/\brounded-2xl\b/);
  await expect(playbackControl).toHaveClass(/\brounded-lg\b/);
  await expectSmoothCorners(card);
  await expectSmoothCorners(playbackControl);
  await expect(
    card.getByRole("button", { name: "Play voice note" }),
  ).toBeVisible();
  const playPauseIcon = card.getByTestId("voice-note-play-pause-icon");
  const primaryIconPath = card.getByTestId("voice-note-play-pause-path-0");
  await expect(playPauseIcon).toHaveAttribute("data-icon-state", "play");
  await expect(playPauseIcon).toHaveCSS("width", "23px");
  await expect(playPauseIcon).toHaveCSS("height", "23px");
  await expect(primaryIconPath).toHaveCSS("fill", /rgb/);
  await expect(primaryIconPath).toHaveCSS("stroke-linejoin", "round");
  const playPath = await primaryIconPath.getAttribute("d");
  await primaryIconPath.evaluate((element) => {
    element.setAttribute("data-morph-identity", "preserved");
  });
  await card.getByRole("button", { name: "Play voice note" }).click();
  await expect(
    card.getByRole("button", { name: "Pause voice note" }),
  ).toBeVisible();
  await expect(playPauseIcon).toHaveAttribute("data-icon-state", "pause");
  await expect(primaryIconPath).toHaveAttribute(
    "data-morph-identity",
    "preserved",
  );
  await expect.poll(() => primaryIconPath.getAttribute("d")).not.toBe(playPath);
  await waitForAnimations(page);
  await card.screenshot({
    path: "test-results/voice-note/voice-note-card-paused.png",
  });
  await card.getByRole("button", { name: "Pause voice note" }).click();
  await page.mouse.move(0, 0);
  await page.evaluate(() => {
    if (document.activeElement instanceof HTMLElement) {
      document.activeElement.blur();
    }
  });
  await expect(
    card.getByRole("slider", { name: "Voice note playback position" }),
  ).toBeVisible();
  await expect(card).toContainText(/\d+:\d{2}/);
  await expect(card).not.toContainText(/· Voice note/);
  await expect(card.getByTestId("voice-note-sender")).toHaveText(
    (
      await page.locator('[data-testid="message-author"]').last().innerText()
    ).trim(),
  );
  // Current / total: the tagged 9.4s is replaced by the real clip length once
  // the media element reports its duration.
  await expect(card.getByTestId("voice-note-time")).toHaveText(
    /^0:00 \/ 0:0\d$/,
  );
  const playbackRate = card.getByTestId("voice-note-playback-rate");
  const playbackRateValue = card.getByTestId("voice-note-playback-rate-value");
  // The speed pill is always visible, not a hover reveal.
  await expect(playbackRate).toHaveCSS("opacity", "1");
  await expect(playbackRate).toHaveAccessibleName(
    "Playback speed 1x; next 1.5x",
  );
  await waitForAnimations(page);
  await expectSmoothCorners(playbackRate);
  await expect(playbackRate).toHaveCSS("padding-left", "10px");
  await expect(playbackRate).toHaveCSS("padding-top", "2px");
  const [rateBackground, playButtonBackground] = await Promise.all([
    playbackRate.evaluate(
      (element) => getComputedStyle(element).backgroundColor,
    ),
    card
      .getByRole("button", { name: "Play voice note" })
      .locator("..")
      .evaluate((element) => getComputedStyle(element).backgroundColor),
  ]);
  expect(rateBackground).toBe(playButtonBackground);
  const widestPlaybackRateWidth = (await playbackRate.boundingBox())?.width;
  expect(widestPlaybackRateWidth).toBeGreaterThan(0);
  await expect(playbackRateValue).toHaveText("1×");
  await playbackRate.click();
  await expect(playbackRateValue).toHaveText("1.5×");
  expect((await playbackRate.boundingBox())?.width).toBe(
    widestPlaybackRateWidth,
  );
  await expect(card.locator("audio")).toHaveJSProperty("playbackRate", 1.5);
  await expect(playbackRate).toHaveAccessibleName(
    "Playback speed 1.5x; next 2x",
  );
  await playbackRate.click();
  await expect(playbackRateValue).toHaveText("2×");
  expect((await playbackRate.boundingBox())?.width).toBe(
    widestPlaybackRateWidth,
  );
  await expect(card.locator("audio")).toHaveJSProperty("playbackRate", 2);
  await playbackRate.click();
  await expect(playbackRateValue).toHaveText("1×");
  await expect(card.locator("audio")).toHaveJSProperty("playbackRate", 1);

  const slider = card.getByRole("slider", {
    name: "Voice note playback position",
  });
  // The range input is the single scrub control: invisible until keyboard
  // focus reaches it, then the waveform shows a focus ring.
  await expect(slider).toHaveCSS("opacity", "0");
  await playbackRate.focus();
  await page.keyboard.press("Shift+Tab");
  await expect(slider).toBeFocused();
  await expect(card.getByTestId("voice-note-playback-waveform")).toHaveCSS(
    "box-shadow",
    /rgb/,
  );
  // Arrow keys scrub in whole seconds (Shift: five), clamped to the clip.
  const readScrub = () =>
    slider.evaluate((element: HTMLInputElement) => ({
      max: Number(element.max),
      value: Number(element.value),
    }));
  const total = (await readScrub()).max;
  // Each scrub is polled with a bound of its own and the reason written down:
  // the position is a media-element read mirrored into React state, so it
  // lands a frame or two after the key, and a loaded runner can outrun
  // Playwright's default expect window.
  const SCRUB_TIMEOUT_MS = 10_000;
  const scrubbedTo = (message: string) =>
    expect.poll(async () => (await readScrub()).value, {
      message,
      timeout: SCRUB_TIMEOUT_MS,
    });
  await page.keyboard.press("ArrowRight");
  await scrubbedTo("ArrowRight steps one second forward").toBeCloseTo(
    Math.min(1, total),
    2,
  );
  await expect(card.getByTestId("voice-note-time")).toHaveText(
    /^0:0\d \/ 0:0\d$/,
  );
  await page.keyboard.press("Home");
  await scrubbedTo("Home returns to the start").toBe(0);
  await page.keyboard.press("Shift+ArrowRight");
  await scrubbedTo("Shift+ArrowRight steps five seconds, clamped").toBeCloseTo(
    Math.min(5, total),
    2,
  );
  await page.keyboard.press("End");
  await scrubbedTo("End goes to the last frame").toBeCloseTo(total, 2);
  // A seek onto the last frame ends the media, and the element fires `ended`
  // for it exactly as it does for playback running out. Whether a paused
  // scrub gets one depends on how far playback reached before the pause, so
  // fire it here rather than race it. That race was this spec's flake, and
  // what it caught was the card rewinding the head on a scrub the user aimed
  // at the end.
  //
  // The painted progress is the witness rather than the slider: the element
  // really is parked at the end, so a `timeupdate` settling after the seek
  // re-mirrors that position into the state either way, while only a rewind
  // repaints the waveform back to the start.
  const progressClipPath = () =>
    card
      .getByTestId("voice-note-progress-waveform")
      .evaluate((element) => getComputedStyle(element).clipPath);
  const audio = card.locator("audio");
  await audio.dispatchEvent("ended");
  await expect
    .poll(progressClipPath, {
      message: "a scrub onto the last frame is not a playthrough",
      timeout: SCRUB_TIMEOUT_MS,
    })
    .not.toContain("100%");
  // Playing out is the other case, and it does rewind to the start.
  await audio.dispatchEvent("play");
  await audio.dispatchEvent("ended");
  await expect
    .poll(progressClipPath, {
      message: "playback running out rewinds the head",
      timeout: SCRUB_TIMEOUT_MS,
    })
    .toContain("100%");
  await page.keyboard.press("Home");
  await expect(card.getByTestId("voice-note-time")).toHaveText(
    /^0:00 \/ 0:0\d$/,
  );
  await slider.evaluate((element: HTMLInputElement) => {
    element.value = String(Number(element.max) / 2);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const progressClip = await card
    .getByTestId("voice-note-progress-waveform")
    .evaluate((element) => getComputedStyle(element).clipPath);
  expect(progressClip).toContain("50%");
  await waitForAnimations(page);
  await card.screenshot({
    path: "test-results/voice-note/voice-note-card.png",
  });
});

test("keeps emoji available but blocks GIFs beside a queued voice note", async ({
  page,
}) => {
  await page.route("http://localhost:3000/info", (route) =>
    route.fulfill({
      body: JSON.stringify({
        gif: {
          provider: "klipy",
          search: "/gifs/search",
          share: "/gifs/share",
        },
        supported_extensions: ["buzz-gif"],
      }),
      contentType: "application/nostr+json",
    }),
  );

  await page.goto("/");
  await page.getByTestId("channel-general").click();
  await startLockedRecording(page);
  await page.waitForTimeout(100);
  await page.getByRole("button", { name: "Send voice note" }).click();
  await expect(page.getByTestId("composer-voice-note-card")).toBeVisible();

  const pickerButton = page.getByTestId("composer-emoji-button");
  await expect(pickerButton).toBeEnabled();
  await expect(pickerButton).toHaveAccessibleName("Insert emoji or GIF");
  await pickerButton.click();
  await expect(page.getByRole("tab", { name: "Emoji" })).toBeEnabled();
  await expect(page.getByRole("tab", { name: "GIFs" })).toBeDisabled();
});

test("starting a duplicate voice-note player pauses the other instance", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("channel-general").click();
  await waitForMockLiveSubscription(page, "general");

  await page.evaluate(
    ({ audioUrl }) => {
      const emit = (
        window as Window & {
          __BUZZ_E2E_EMIT_MOCK_MESSAGE__?: (input: {
            channelName: string;
            content: string;
            extraTags: string[][];
          }) => unknown;
        }
      ).__BUZZ_E2E_EMIT_MOCK_MESSAGE__;
      if (!emit) throw new Error("Mock message emitter is unavailable.");
      const input = {
        channelName: "general",
        content: `[voice-note-123.mp4](${audioUrl})`,
        extraTags: [
          [
            "imeta",
            `url ${audioUrl}`,
            "m video/mp4",
            "duration 9.4",
            "filename voice-note-123.mp4",
          ],
        ],
      };
      emit(input);
      emit(input);
    },
    { audioUrl: AUDIO_URL },
  );

  const cards = page.getByTestId("audio-message-attachment");
  await expect(cards).toHaveCount(2);
  const firstAudio = cards.nth(0).locator("audio");
  const secondAudio = cards.nth(1).locator("audio");

  await cards.nth(0).getByRole("button", { name: "Play voice note" }).click();
  await expect(firstAudio).toHaveJSProperty("paused", false);
  await cards.nth(1).getByRole("button", { name: "Play voice note" }).click();
  await expect(firstAudio).toHaveJSProperty("paused", true);
  await expect(secondAudio).toHaveJSProperty("paused", false);
});

test("received card titles the sender and folds the transcript in a channel", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("channel-general").click();
  await waitForMockLiveSubscription(page, "general");
  const transcript = "Status is **green** on the Studio. Say the word.";
  const caption = "Hey **team**, here is the update";
  await emitVoiceNoteMessage(page, "general", AUDIO_URL, transcript, {
    caption,
    imageUrl: IMAGE_URL,
  });

  const card = page.getByTestId("audio-message-attachment").last();
  await expect(card).toBeVisible();
  const row = page.locator("[data-message-id]").filter({ has: card });
  const author = (
    await row.locator('[data-testid="message-author"]').first().innerText()
  ).trim();
  expect(author.length).toBeGreaterThan(0);
  await expect(card.getByTestId("voice-note-sender")).toHaveText(author);
  // Current / total: the tagged 9.4s is replaced by the real clip length once
  // the media element reports its duration.
  await expect(card.getByTestId("voice-note-time")).toHaveText(
    /^0:00 \/ 0:0\d$/,
  );
  // The caption is body prose: rendered as Markdown above the cards, not
  // demoted into the transcript row. The image beside the note is its own
  // card, and only one voice-note card is drawn.
  await expect(row.getByText("here is the update")).toHaveCount(1);
  await expect(row.locator("strong").filter({ hasText: "team" })).toHaveCount(
    1,
  );
  await expect(card.getByText("here is the update")).toHaveCount(0);
  await expect(row.getByTestId("message-image-lightbox-trigger")).toHaveCount(
    1,
  );
  await expect(row.getByTestId("audio-message-attachment")).toHaveCount(1);

  const toggle = card.getByRole("button", { name: "Transcript" });
  const text = card.getByTestId("voice-note-transcript-text");
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(toggle).toHaveAttribute("aria-controls", /.+/);
  await expect(text).toBeHidden();
  await expect(text).toHaveAttribute(
    "id",
    (await toggle.getAttribute("aria-controls")) ?? "",
  );

  await toggle.focus();
  await page.keyboard.press("Enter");
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(text).toBeVisible();
  // The transcript renders through Markdown too: emphasis is real emphasis,
  // not literal asterisks.
  await expect(text).toHaveText("Status is green on the Studio. Say the word.");
  await expect(text.locator("strong")).toHaveText("green");
  expect(
    await page.evaluate(() =>
      window.localStorage.getItem("buzz.voiceNote.transcriptOpen"),
    ),
  ).toBe("open");
  await waitForAnimations(page);
  await card.screenshot({
    path: "test-results/voice-note/voice-note-card-transcript.png",
  });

  // The last choice survives a reload.
  await page.reload();
  await page.getByTestId("channel-general").click();
  await waitForMockLiveSubscription(page, "general");
  await emitVoiceNoteMessage(page, "general", AUDIO_URL, transcript);
  const reloadedCard = page.getByTestId("audio-message-attachment").last();
  await expect(
    reloadedCard.getByRole("button", { name: "Transcript" }),
  ).toHaveAttribute("aria-expanded", "true");
  await reloadedCard.getByRole("button", { name: "Transcript" }).click();
  expect(
    await page.evaluate(() =>
      window.localStorage.getItem("buzz.voiceNote.transcriptOpen"),
    ),
  ).toBe("closed");

  // No transcript on the attachment: no Transcript row, even with a caption.
  await emitVoiceNoteMessage(page, "general", AUDIO_URL, undefined, {
    caption: "Just a caption",
  });
  const bareCard = page.getByTestId("audio-message-attachment").last();
  await expect(bareCard).toBeVisible();
  await expect(bareCard.getByTestId("voice-note-transcript")).toHaveCount(0);
  const bareRow = page.locator("[data-message-id]").filter({ has: bareCard });
  await expect(bareRow.getByText("Just a caption")).toHaveCount(1);
  await expect(bareCard.getByText("Just a caption")).toHaveCount(0);
});

test("received card opens the transcript by default in a DM", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByTestId("dm-list").getByTestId("channel-alice-tyler").click();
  await waitForMockLiveSubscription(page, "alice-tyler");
  await emitVoiceNoteMessage(
    page,
    "alice-tyler",
    AUDIO_URL,
    "Chief, it's Neo. Two things need your eyes.",
  );

  const card = page.getByTestId("audio-message-attachment").last();
  await expect(card).toBeVisible();
  const toggle = card.getByRole("button", { name: "Transcript" });
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(card.getByTestId("voice-note-transcript-text")).toBeVisible();
});
