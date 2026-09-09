import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { waitForAnimations } from "../helpers/animations";
import { installMockBridge } from "../helpers/bridge";

const AUDIO_URL = "http://127.0.0.1:4173/sounds/ping.mp3";
const SHOTS = "test-results/voice-note-recorder";
/** Mirrors VOICE_NOTE_TAP_TO_LOCK_MS: a shorter press is a tap. */
const TAP_TO_LOCK_MS = 300;
const HOLD_MS = TAP_TO_LOCK_MS + 150;

type ClockWindow = Window & {
  __BUZZ_E2E_ADVANCE_CLOCK__?: (ms: number) => void;
  __BUZZ_E2E_MEDIA_RECORDER_STOPS__?: number;
};

/**
 * Recorder states from design/voice-notes/DesktopComposer.dc.html: hold the
 * mic (pointer) or Space (keyboard) to record, release sends, a tap locks
 * hands free, Esc discards, L, the chip or a slide onto it locks, locked rows
 * pause, resume, trash and Send. Every path is exercised through the modality
 * that reaches it (rule 8), including the ones that are not the primary one.
 */

function recorder(page: Page): Locator {
  return page.getByTestId("voice-note-recorder");
}

function sentCards(page: Page): Locator {
  return page.getByTestId("audio-message-attachment");
}

function liveStatus(page: Page): Locator {
  return page.getByTestId("voice-note-live-status");
}

/**
 * A toast, scoped to the toaster. An unscoped `getByText` matches the same
 * words anywhere on the page, which TESTING.md calls a strict-mode flake.
 */
function toastWithText(page: Page, text: string): Locator {
  return page.locator("[data-sonner-toast]").filter({ hasText: text });
}

function activeElement(page: Page) {
  return page.evaluate(() => {
    const element = document.activeElement;
    return element === null || element === document.body
      ? "body"
      : (element.getAttribute("data-testid") ??
          element.getAttribute("aria-label") ??
          element.tagName);
  });
}

async function openGeneral(page: Page) {
  await page.goto("/");
  const channel = page.getByTestId("channel-general");
  await expect(channel).toBeVisible({ timeout: 10_000 });
  await channel.click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

/** Press the mic without releasing; resolves once the stream is live. */
async function pressMic(page: Page) {
  const mic = page.getByRole("button", { name: "Record voice note" });
  const box = await mic.boundingBox();
  if (!box) throw new Error("The mic button has no layout box.");
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await expect(recorder(page)).toBeVisible();
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "recording",
  );
}

/** Press the mic and keep it down long enough to be a hold, not a tap. */
async function holdMic(page: Page) {
  await pressMic(page);
  await page.waitForTimeout(HOLD_MS);
}

/**
 * Enter on the focused mic. The mic's tooltip opened on focus and is still
 * mounted for its close animation once focus moves to the editor; while it
 * lives it is the topmost Radix layer and takes Escape, so wait for it to
 * leave before pressing keys that must reach the recorder from outside the
 * editor (the editor's own key path is unaffected).
 */
async function startHandsFree(page: Page) {
  await page.getByRole("button", { name: "Record voice note" }).focus();
  await page.keyboard.press("Enter");
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await expect(page.locator("[role=tooltip]")).toHaveCount(0);
}

/**
 * A sent note is only on screen once the recorder has encoded it, the upload
 * has resolved and the published message has echoed back through the bridge.
 * The live region says "Voice note sent" at the start of that chain, not the
 * end, so the card needs a bound of its own rather than Playwright's default
 * 5s expect window, which a loaded runner can overrun.
 */
const SENT_CARD_TIMEOUT_MS = 15_000;

async function expectSent(page: Page, count = 1) {
  await expect(recorder(page)).toHaveCount(0, {
    timeout: SENT_CARD_TIMEOUT_MS,
  });
  await expect(sentCards(page)).toHaveCount(count, {
    timeout: SENT_CARD_TIMEOUT_MS,
  });
  await expect(page.getByTestId("composer-voice-note-card")).toHaveCount(0);
}

async function mediaRecorderStops(page: Page): Promise<number> {
  return page.evaluate(
    () => (window as ClockWindow).__BUZZ_E2E_MEDIA_RECORDER_STOPS__ ?? 0,
  );
}

/** Seeds a message with one reply so `#general` has a thread to open. */
async function seedThreadRoot(page: Page): Promise<string> {
  await expect
    .poll(() =>
      page.evaluate(
        () => typeof window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__ === "function",
      ),
    )
    .toBe(true);
  const rootId = await page.evaluate(() => {
    const root = window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
      channelName: "general",
      content: "Voice note thread root",
      createdAt: 1_700_900_000,
    });
    if (!root) throw new Error("Failed to seed the thread root.");
    window.__BUZZ_E2E_EMIT_MOCK_MESSAGE__?.({
      channelName: "general",
      content: "A reply, so the root shows a thread summary",
      createdAt: 1_700_900_001,
      parentEventId: root.id,
    });
    return root.id;
  });
  await expect(
    page.locator(
      `[data-testid="message-thread-summary"][data-thread-head-id="${rootId}"]`,
    ),
  ).toBeVisible();
  return rootId;
}

async function blurActiveElement(page: Page) {
  await page.evaluate(() => {
    if (document.activeElement instanceof HTMLElement) {
      document.activeElement.blur();
    }
  });
}

test.beforeEach(async ({ page }) => {
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
    // Count real MediaRecorder stops so a discard can be proven to reach the
    // recorder, not just the row.
    const clockWindow = window as ClockWindow;
    clockWindow.__BUZZ_E2E_MEDIA_RECORDER_STOPS__ = 0;
    const originalStop = MediaRecorder.prototype.stop;
    MediaRecorder.prototype.stop = function stop() {
      clockWindow.__BUZZ_E2E_MEDIA_RECORDER_STOPS__ =
        (clockWindow.__BUZZ_E2E_MEDIA_RECORDER_STOPS__ ?? 0) + 1;
      return originalStop.call(this);
    };
    // The recorder's elapsed clock is `performance.now()`; letting the spec
    // move it forward reaches the 5:00 cap without a five-minute wait.
    //
    // Installed lazily, on the first advance. React's scheduler calls
    // `performance.now` constantly, so replacing it with a JS closure costs
    // the whole page real time (app boot measured ~600ms shimmed against
    // ~420ms native here). Paying that in the twelve specs that never touch
    // the clock is what pushed this file over Playwright's default expect
    // window on a loaded runner and made it flake. Only the cap spec pays it
    // now, and only from the moment it asks.
    let clockOffset = 0;
    const realNow = performance.now.bind(performance);
    clockWindow.__BUZZ_E2E_ADVANCE_CLOCK__ = (ms) => {
      if (clockOffset === 0) performance.now = () => realNow() + clockOffset;
      clockOffset += ms;
    };
  });

  await installMockBridge(page, {
    deferredComposerUploads: true,
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

test("holding the mic records and releasing sends", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.setViewportSize({ width: 1280, height: 720 });
  await openGeneral(page);
  await expect(sentCards(page)).toHaveCount(0);

  // Idle: mic in the toolbar, send disabled. Typing lights send and keeps
  // the mic where it is (no slot swap on desktop).
  const mic = page.getByRole("button", { name: "Record voice note" });
  const send = page.getByTestId("send-message");
  await expect(send).toBeDisabled();
  await page.getByTestId("message-input").click();
  await page.keyboard.type("Send me a voice note");
  await expect(send).toBeEnabled();
  await expect(mic).toBeVisible();
  await page.keyboard.press("ControlOrMeta+A");
  await page.keyboard.press("Backspace");
  await expect(send).toBeDisabled();

  await pressMic(page);
  const row = recorder(page);
  await expect(
    row.getByRole("button", { name: "Discard voice note" }),
  ).toBeVisible();
  await expect(row.getByTestId("voice-note-live-dot")).toBeVisible();
  await expect(row).toContainText(/\d+:\d{2} \/ 5:00/);
  // During a pointer hold the chip is a slide target, never a button: the
  // mouse button is down on the mic, so a click would only ever send.
  const chip = row.getByTestId("voice-note-lock-chip");
  await expect(chip).toBeVisible();
  await expect(chip).toHaveAttribute("data-locked", "false");
  await expect(chip).toHaveAttribute("data-lock-target", "slide");
  await expect(row.getByRole("button", { name: /lock/ })).toHaveCount(0);
  await expect(row.getByTestId("voice-note-pause-resume")).toHaveCount(0);
  await expect(liveStatus(page)).toHaveText(
    "Recording voice note, release to send",
  );
  // The send slot stays a compact Send while the hold lasts.
  const holdSend = page.getByRole("button", { name: "Send voice note" });
  await expect(holdSend).toBeEnabled();
  await expect(holdSend).not.toHaveAttribute("data-voice-note-locked", "true");
  await page.waitForTimeout(HOLD_MS);
  await waitForAnimations(page);
  await page.getByTestId("message-composer-toolbar").screenshot({
    path: `${SHOTS}/recording.png`,
  });

  await page.mouse.up();
  await expectSent(page);
  await expect(liveStatus(page)).toHaveText("Voice note sent");
});

test("a tap on the mic records hands free instead of sending a blip", async ({
  page,
}) => {
  await openGeneral(page);
  await page.getByRole("button", { name: "Record voice note" }).click();
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await expect(sentCards(page)).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Send voice note" }),
  ).toHaveAttribute("data-voice-note-locked", "true");

  // The same from the keyboard: a Space tap in the empty editor.
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await page.getByTestId("message-input").click();
  await page.keyboard.down("Space");
  await expect(recorder(page)).toBeVisible();
  await page.keyboard.up("Space");
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await expect(sentCards(page)).toHaveCount(0);
  await page.waitForTimeout(150);
  await page.getByTestId("send-voice-note").click();
  await expectSent(page);
});

test("holding Space in an empty editor records; Shift+Space types a space", async ({
  page,
}) => {
  await openGeneral(page);
  const input = page.getByTestId("message-input");
  await input.click();

  // Shift+Space is not Space: it types and never starts a recording.
  await page.keyboard.press("Shift+Space");
  await expect(recorder(page)).toHaveCount(0);
  // ProseMirror renders a trailing space as a non-breaking space.
  expect(await input.evaluate((element) => element.textContent)).toMatch(
    /[  ]/,
  );
  await page.keyboard.press("ControlOrMeta+A");
  await page.keyboard.press("Backspace");

  await page.keyboard.down("Space");
  await expect(recorder(page)).toBeVisible();
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "recording",
  );
  // A held key auto-repeats: Playwright marks a second `down` as a repeat.
  // The repeats must be swallowed, not typed into the editor.
  await page.keyboard.down("Space");
  await page.keyboard.down("Space");
  await page.waitForTimeout(HOLD_MS);
  expect(await input.evaluate((element) => element.textContent)).toBe("");
  await page.keyboard.up("Space");
  await expectSent(page);

  // With text in the editor, Space is just a space.
  await input.click();
  await page.keyboard.type("hello");
  await page.keyboard.down("Space");
  await page.keyboard.up("Space");
  await expect(recorder(page)).toHaveCount(0);
  await expect(input).toHaveText(/hello\s/);
});

test("Space beside another attachment types a space without a toast", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.localStorage.setItem("buzz.voiceNote.reviewBeforeSend", "on");
  });
  await openGeneral(page);
  await startHandsFree(page);
  await page.waitForTimeout(150);
  await page.getByTestId("send-voice-note").click();
  await expect(page.getByTestId("composer-voice-note-card")).toBeVisible();

  const input = page.getByTestId("message-input");
  await input.click();
  await page.keyboard.press("Space");
  await expect(recorder(page)).toHaveCount(0);
  expect(await input.evaluate((element) => element.textContent)).toMatch(
    /[  ]/,
  );
  await expect(
    page.getByText("A voice note must be the only attachment."),
  ).toHaveCount(0);
});

test("IME composition keys never reach the recorder", async ({ page }) => {
  await openGeneral(page);
  const input = page.getByTestId("message-input");
  await input.click();
  // A CJK IME selects a candidate with Space while composing; the event
  // arrives with `isComposing` set and must stay with the editor.
  await input.evaluate((element) => {
    element.dispatchEvent(
      new KeyboardEvent("keydown", {
        bubbles: true,
        cancelable: true,
        code: "Space",
        isComposing: true,
        key: " ",
      }),
    );
  });
  await page.waitForTimeout(200);
  await expect(recorder(page)).toHaveCount(0);

  // Likewise L while composing during a keyboard hold must not lock.
  await page.keyboard.down("Space");
  await expect(recorder(page)).toBeVisible();
  await input.evaluate((element) => {
    element.dispatchEvent(
      new KeyboardEvent("keydown", {
        bubbles: true,
        cancelable: true,
        code: "KeyL",
        isComposing: true,
        key: "l",
      }),
    );
  });
  await page.waitForTimeout(100);
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "recording",
  );
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await page.keyboard.up("Space");
});

test("Escape discards a held recording without sending", async ({ page }) => {
  await openGeneral(page);
  await holdMic(page);
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await page.mouse.up();
  await page.waitForTimeout(200);
  await expect(sentCards(page)).toHaveCount(0);
  await expect(page.getByTestId("composer-voice-note-card")).toHaveCount(0);
  await expect(liveStatus(page)).toHaveText("Voice note discarded");

  // The same from a keyboard hold.
  await page.getByTestId("message-input").click();
  await page.keyboard.down("Space");
  await expect(recorder(page)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await page.keyboard.up("Space");
  await page.waitForTimeout(200);
  await expect(sentCards(page)).toHaveCount(0);
});

test("Escape discards from any focus and never marks the channel read", async ({
  page,
}) => {
  await openGeneral(page);
  const channel = page.getByTestId("channel-general");
  // Make the app-level Escape shortcut (mark channel read) observable: the
  // active channel is bold while it is unread.
  await channel.click({ button: "right" });
  await page.getByText("Mark unread").click();
  await expect(channel).toHaveCSS("font-weight", "700");

  // Enter on the mic starts hands free and hands focus to the editor: the
  // mic is leaving the toolbar, and its tooltip must not eat the first Esc.
  await startHandsFree(page);
  await expect(page.getByTestId("message-input")).toBeFocused();
  const stopsBefore = await mediaRecorderStops(page);
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await expect.poll(() => mediaRecorderStops(page)).toBe(stopsBefore + 1);
  await expect(sentCards(page)).toHaveCount(0);
  await expect(channel).toHaveCSS("font-weight", "700");

  // Focus on a recorder control (the pause button of a locked note). Its
  // tooltip opens on keyboard focus and is a document-level Radix layer that
  // would otherwise eat the key, so the row claims Escape in the window
  // capture phase for its own controls: one press discards, and focus lands
  // back in the editor rather than on body. The mark-read shortcut never runs.
  await startHandsFree(page);
  await recorder(page).getByTestId("voice-note-pause-resume").focus();
  expect(await activeElement(page)).toBe("voice-note-pause-resume");
  const pauseTooltip = page.getByRole("tooltip", { name: "Pause recording" });
  await expect(pauseTooltip).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await expect(page.getByTestId("message-input")).toBeFocused();
  await expect(channel).toHaveCSS("font-weight", "700");

  // The trash is a tooltip trigger too, and a second press landing inside
  // the tooltip's exit animation used to be swallowed as well.
  await startHandsFree(page);
  await recorder(page).getByTestId("voice-note-discard").focus();
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await expect(channel).toHaveCSS("font-weight", "700");

  // The Send slot with focus: same, and Send is not a tooltip trigger, so a
  // single Esc discards.
  await startHandsFree(page);
  await page.getByTestId("send-voice-note").focus();
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await expect(channel).toHaveCSS("font-weight", "700");

  // Focus on nothing at all (a pointer hold with the editor blurred).
  await blurActiveElement(page);
  await holdMic(page);
  expect(await activeElement(page)).toBe("body");
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await page.mouse.up();
  await expect(channel).toHaveCSS("font-weight", "700");
  await expect(sentCards(page)).toHaveCount(0);

  // Control: with no recorder, the same Esc is the mark-read shortcut.
  await blurActiveElement(page);
  await page.keyboard.press("Escape");
  await expect(channel).toHaveCSS("font-weight", "400");
});

test("L locks a hold; locked rows pause, resume, trash and Send", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.setViewportSize({ width: 1280, height: 720 });
  await openGeneral(page);

  // Pointer hold with focus outside the editor: L is the lock shortcut.
  await blurActiveElement(page);
  await pressMic(page);
  await page.keyboard.press("l");
  const row = recorder(page);
  await expect(row).toHaveAttribute("data-voice-note-state", "locked");
  const chip = row.getByTestId("voice-note-lock-chip");
  await expect(chip).toHaveAttribute("data-locked", "true");
  await expect(chip).toHaveText(/Locked/);
  await expect(liveStatus(page)).toHaveText("Recording locked, hands free");
  // Releasing the pointer after the lock keeps recording.
  await page.mouse.up();
  await expect(row).toHaveAttribute("data-voice-note-state", "locked");
  const send = page.getByRole("button", { name: "Send voice note" });
  await expect(send).toHaveAttribute("data-voice-note-locked", "true");
  await expect(send).toHaveText("Send");
  await expect(row.getByTestId("voice-note-live-dot")).toHaveCount(0);
  const discard = row.getByRole("button", { name: "Discard voice note" });
  await expect(discard).toBeVisible();
  // Locked: the discard is a trash can, drawn in the destructive colour.
  await expect(discard).toHaveClass(/text-destructive/);
  await page.waitForTimeout(250);
  await waitForAnimations(page);
  await page.getByTestId("message-composer-toolbar").screenshot({
    path: `${SHOTS}/locked.png`,
  });

  // Pause stops the clock and the waveform; resume continues the same note.
  const pause = row.getByRole("button", { name: "Pause recording" });
  await pause.click();
  await expect(row).toHaveAttribute("data-voice-note-state", "paused");
  await expect(liveStatus(page)).toHaveText("Recording paused");
  const resume = row.getByRole("button", { name: "Resume recording" });
  await expect(resume).toBeVisible();
  const pausedTime = await row.locator("span.tabular-nums").first().innerText();
  const pausedBars = await row
    .getByTestId("voice-note-live-waveform")
    .locator("[data-recorded-sample]")
    .count();
  await page.waitForTimeout(400);
  expect(await row.locator("span.tabular-nums").first().innerText()).toBe(
    pausedTime,
  );
  expect(
    await row
      .getByTestId("voice-note-live-waveform")
      .locator("[data-recorded-sample]")
      .count(),
  ).toBe(pausedBars);
  await waitForAnimations(page);
  await page.getByTestId("message-composer-toolbar").screenshot({
    path: `${SHOTS}/paused.png`,
  });
  await resume.focus();
  await page.keyboard.press("Enter");
  await expect(row).toHaveAttribute("data-voice-note-state", "locked");
  await expect(liveStatus(page)).toHaveText("Recording resumed");

  // Enter on the trash discards the locked note; focus returns to the
  // editor instead of dying with the row.
  await discard.focus();
  await page.keyboard.press("Enter");
  await expect(recorder(page)).toHaveCount(0);
  await expect(sentCards(page)).toHaveCount(0);
  await expect(page.getByTestId("message-input")).toBeFocused();

  // During a Space hold the chip is a real button: Enter on it locks and
  // focus moves to the pause control that takes its place. Held L
  // auto-repeats must not type "l" either.
  await page.keyboard.down("Space");
  await expect(recorder(page)).toBeVisible();
  const pressChip = recorder(page).getByRole("button", {
    name: "Press L to lock",
  });
  await expect(pressChip).toHaveAttribute("data-lock-target", "press");
  await pressChip.focus();
  await page.keyboard.press("Enter");
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  expect(await activeElement(page)).toBe("voice-note-pause-resume");
  await page.keyboard.up("Space");
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await page.waitForTimeout(150);
  await send.focus();
  await page.keyboard.press("Enter");
  await expectSent(page);

  // Held L during a Space hold: the first press locks, the auto-repeats are
  // swallowed rather than typed.
  const input = page.getByTestId("message-input");
  await input.click();
  await page.keyboard.down("Space");
  await expect(recorder(page)).toBeVisible();
  await page.keyboard.down("l");
  await page.keyboard.down("l");
  await page.keyboard.down("l");
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await page.keyboard.up("l");
  await page.keyboard.up("Space");
  expect(await input.evaluate((element) => element.textContent)).toBe("");
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await page.waitForTimeout(150);
  await send.click();
  await expectSent(page, 2);
});

test("typing a word with an l during a pointer hold types, and sliding onto the chip locks", async ({
  page,
}) => {
  await openGeneral(page);
  const input = page.getByTestId("message-input");
  await input.click();
  await pressMic(page);
  // Focus is still in the editor: "l" is a letter here, not a shortcut.
  await page.keyboard.type("hello");
  await expect(input).toHaveText("hello");
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "recording",
  );

  // Slide the held pointer onto the chip: that is the lock for a mouse.
  const chip = recorder(page).getByTestId("voice-note-lock-chip");
  const box = await chip.boundingBox();
  if (!box) throw new Error("The lock chip has no layout box.");
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, {
    steps: 8,
  });
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await page.mouse.up();
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await expect(sentCards(page)).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
});

test("losing the hold to blur or a hidden page releases it like a pointer release", async ({
  page,
}) => {
  await openGeneral(page);
  await holdMic(page);
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await expectSent(page);
  await page.mouse.up();

  // A locked note is hands free: blur leaves it running.
  await blurActiveElement(page);
  await pressMic(page);
  await page.keyboard.press("l");
  await page.mouse.up();
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await page.waitForTimeout(200);
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);

  // Focus leaving the composer during a Space hold releases the keyboard
  // hold (rule 8): the sidebar takes focus, the note sends.
  await page.getByTestId("message-input").click();
  await page.keyboard.down("Space");
  await expect(recorder(page)).toBeVisible();
  await page.waitForTimeout(HOLD_MS);
  await page.getByTestId("channel-general").focus();
  await expectSent(page, 2);
  await page.keyboard.up("Space");

  // The page going hidden mid-hold ends it the same way.
  await holdMic(page);
  await page.evaluate(() => {
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "hidden",
    });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expectSent(page, 3);
  await page.mouse.up();
});

test("the 5:00 cap sends a locked note and waits while it is paused", async ({
  page,
}) => {
  // The clock jumps below confuse spring exits; zero-length exits keep the
  // row's departure observable.
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openGeneral(page);
  await startHandsFree(page);
  const row = recorder(page);
  const advance = (ms: number) =>
    page.evaluate(
      (delta) => (window as ClockWindow).__BUZZ_E2E_ADVANCE_CLOCK__?.(delta),
      ms,
    );

  // 4:00, not 4:58: the clock only jumps on demand, so the seconds between
  // the jump and the assertions are real ones counted against the cap. Two
  // seconds of headroom is not a margin on a loaded runner; sixty is.
  await advance(240_000);
  await expect(row).toContainText(/4:0\d \/ 5:00/);
  await expect(row).toHaveAttribute("data-voice-note-state", "locked");

  // Paused: wall time passes, recorded time does not, so no cap.
  await row.getByRole("button", { name: "Pause recording" }).click();
  await expect(row).toHaveAttribute("data-voice-note-state", "paused");
  const pausedTime = await row.locator("span.tabular-nums").first().innerText();
  await advance(120_000);
  await page.waitForTimeout(400);
  await expect(row).toHaveAttribute("data-voice-note-state", "paused");
  expect(await row.locator("span.tabular-nums").first().innerText()).toBe(
    pausedTime,
  );
  await expect(sentCards(page)).toHaveCount(0);

  // Resumed: the last second of recorded time reaches the cap and sends.
  await row.getByRole("button", { name: "Resume recording" }).click();
  await expect(row).toHaveAttribute("data-voice-note-state", "locked");
  await expect(row).toContainText(/4:0\d \/ 5:00/);
  await advance(61_000);
  await expectSent(page);
});

test("the review setting holds the note in the composer with listen back, re-record and send", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.localStorage.setItem("buzz.voiceNote.reviewBeforeSend", "on");
  });
  await openGeneral(page);

  await holdMic(page);
  await page.mouse.up();
  const composerCard = page.getByTestId("composer-voice-note-card");
  await expect(composerCard).toBeVisible();
  await expect(liveStatus(page)).toHaveText("Voice note ready to review");
  await expect(sentCards(page)).toHaveCount(0);
  await expect(
    composerCard.getByRole("button", { name: "Play voice note" }),
  ).toBeVisible();
  const review = page.getByTestId("voice-note-review");
  await expect(review.getByRole("button", { name: "Re-record" })).toBeVisible();
  await expect(
    review.getByRole("button", { name: "Send voice note" }),
  ).toBeVisible();

  // Re-record drops the take and starts a hands-free recording.
  await review.getByRole("button", { name: "Re-record" }).click();
  await expect(composerCard).toHaveCount(0);
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await page.waitForTimeout(150);
  await page.getByTestId("send-voice-note").click();
  await expect(composerCard).toBeVisible();

  await review.getByRole("button", { name: "Send voice note" }).click();
  await expectSent(page);
});

test("the review setting is read when the note finishes, not when it starts", async ({
  page,
}) => {
  await openGeneral(page);
  await startHandsFree(page);
  await page.waitForTimeout(150);
  // Another window turns review on mid-recording; localStorage reports it
  // here through the storage event.
  await page.evaluate(() => {
    window.localStorage.setItem("buzz.voiceNote.reviewBeforeSend", "on");
    window.dispatchEvent(
      new StorageEvent("storage", {
        key: "buzz.voiceNote.reviewBeforeSend",
        newValue: "on",
        storageArea: window.localStorage,
      }),
    );
  });
  await page.getByTestId("send-voice-note").click();
  await expect(page.getByTestId("composer-voice-note-card")).toBeVisible();
  await expect(sentCards(page)).toHaveCount(0);

  // And back off: the next note sends straight away again.
  await page.getByTestId("voice-note-review-send").click();
  await expectSent(page);
  await page.evaluate(() => {
    window.localStorage.setItem("buzz.voiceNote.reviewBeforeSend", "off");
    window.dispatchEvent(
      new StorageEvent("storage", {
        key: "buzz.voiceNote.reviewBeforeSend",
        newValue: "off",
        storageArea: window.localStorage,
      }),
    );
  });
  await startHandsFree(page);
  await page.waitForTimeout(150);
  await page.getByTestId("send-voice-note").click();
  await expectSent(page, 2);
});

test("Escape closes the mention list before it reaches the recording", async ({
  page,
}) => {
  await openGeneral(page);
  await startHandsFree(page);
  const input = page.getByTestId("message-input");
  await input.click();
  await input.pressSequentially("@al");
  const mentions = page.getByTestId("mention-autocomplete");
  await expect(mentions).toBeVisible();

  // The recorder's editor key path runs before the mention handler, so it has
  // to decline while a list is open instead of taking the key for itself.
  const stopsBefore = await mediaRecorderStops(page);
  await page.keyboard.press("Escape");
  await expect(mentions).toHaveCount(0);
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  expect(await mediaRecorderStops(page)).toBe(stopsBefore);
  await expect(liveStatus(page)).not.toHaveText("Voice note discarded");

  // With the list gone the same key discards, as it does anywhere else.
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await expect(liveStatus(page)).toHaveText("Voice note discarded");
});

test("only one composer in the window can record", async ({ page }) => {
  await openGeneral(page);
  const threadId = await seedThreadRoot(page);
  const summary = page.locator(
    `[data-testid="message-thread-summary"][data-thread-head-id="${threadId}"]`,
  );
  await summary.click();
  const threadPanel = page.getByTestId("message-thread-panel");
  await expect(threadPanel).toBeVisible();
  const threadInput = threadPanel.getByTestId("message-input");
  await expect(threadInput).toBeVisible();

  // Record in the channel composer, beside the docked thread's own composer.
  await page.getByRole("button", { name: "Record voice note" }).first().click();
  await expect(recorder(page)).toHaveCount(1);

  // The thread's mic is disabled while the microphone is taken, and Space in
  // its empty editor types a space instead of opening a second recorder.
  const threadMic = threadPanel.getByRole("button", {
    name: "Record voice note",
  });
  await expect(threadMic).toBeDisabled();
  await threadInput.click();
  await page.keyboard.press("Space");
  await expect(recorder(page)).toHaveCount(1);
  // ProseMirror renders a trailing space as a non-breaking space.
  expect(await threadInput.evaluate((element) => element.textContent)).toMatch(
    /[  ]/,
  );
  // Two composers, two live regions, but only one of them may be announcing.
  const statuses = page.getByTestId("voice-note-live-status");
  await expect(statuses).toHaveCount(2);
  await expect(statuses.filter({ hasText: /Recording/ })).toHaveCount(1);

  // ProseMirror marks Escape handled at the contenteditable, so the sibling
  // recording is only reachable because the composer key path asks the
  // registry who is recording.
  await page.keyboard.press("ControlOrMeta+A");
  await page.keyboard.press("Backspace");
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await expect(threadPanel).toBeVisible();
  await expect(sentCards(page)).toHaveCount(0);

  // The microphone is free again, and now the thread composer may take it.
  await expect(threadMic).toBeEnabled();
  await threadMic.click();
  await expect(threadPanel.getByTestId("voice-note-recorder")).toHaveCount(1);
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
});

test("re-recording while another composer holds the microphone keeps the note", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.localStorage.setItem("buzz.voiceNote.reviewBeforeSend", "on");
  });
  await openGeneral(page);
  const threadId = await seedThreadRoot(page);

  // A finished take waiting in the channel composer's review row.
  await holdMic(page);
  await page.mouse.up();
  const composerCard = page.getByTestId("composer-voice-note-card");
  await expect(composerCard).toBeVisible();
  const review = page.getByTestId("voice-note-review");
  const rerecord = review.getByRole("button", { name: "Re-record" });
  await expect(rerecord).toBeVisible();

  // The docked thread composer takes the microphone beside it.
  await page
    .locator(
      `[data-testid="message-thread-summary"][data-thread-head-id="${threadId}"]`,
    )
    .click();
  const threadPanel = page.getByTestId("message-thread-panel");
  await expect(threadPanel).toBeVisible();
  const threadMic = threadPanel.getByRole("button", {
    name: "Record voice note",
  });
  await threadMic.click();
  await expect(threadPanel.getByTestId("voice-note-recorder")).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );

  // Re-record has to refuse before it destroys anything: the queued file is
  // the only copy of the audio, and the start behind it cannot succeed.
  await rerecord.click();
  await expect(
    toastWithText(page, "Finish or discard the other voice note first."),
  ).toBeVisible();
  await expect(composerCard).toBeVisible();
  await expect(review).toBeVisible();
  await expect(page.getByTestId("voice-note-recorder")).toHaveCount(1);
  await expect(sentCards(page)).toHaveCount(0);

  // And it is a refusal, not a dead button: the microphone comes back.
  await threadPanel.getByTestId("voice-note-discard").click();
  await expect(threadPanel.getByTestId("voice-note-recorder")).toHaveCount(0);
  await rerecord.click();
  await expect(composerCard).toHaveCount(0);
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
});

test("a start that never opens the microphone frees it for the next one", async ({
  page,
}) => {
  // No `MediaRecorder` in this webview: `start` returns without the recorder
  // ever leaving idle, so nothing keyed on a status change can clean up after
  // it. Registered after the beforeEach hook, so the restored constructor is
  // the instrumented one.
  await page.addInitScript(() => {
    const real = window.MediaRecorder;
    (
      window as Window & { MediaRecorder?: typeof MediaRecorder }
    ).MediaRecorder = undefined;
    (
      window as Window & { __BUZZ_E2E_RESTORE_MEDIA_RECORDER__?: () => void }
    ).__BUZZ_E2E_RESTORE_MEDIA_RECORDER__ = () => {
      window.MediaRecorder = real;
    };
  });
  await openGeneral(page);

  const mic = page.getByRole("button", { name: "Record voice note" });
  await mic.click();
  await expect(
    toastWithText(
      page,
      "Voice recording is not available in this environment.",
    ),
  ).toBeVisible();
  await expect(recorder(page)).toHaveCount(0);

  // The failed start must not still be holding the microphone: a claim with
  // no recording behind it disables every other composer's mic for the life
  // of the page and swallows Escape with it.
  await page.evaluate(() =>
    (
      window as Window & { __BUZZ_E2E_RESTORE_MEDIA_RECORDER__?: () => void }
    ).__BUZZ_E2E_RESTORE_MEDIA_RECORDER__?.(),
  );
  await mic.click();
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await expect(
    toastWithText(page, "Finish or discard the other voice note first."),
  ).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await expect(liveStatus(page)).toHaveText("Voice note discarded");
});

test("a sibling composer's Escape cannot cancel a note that is already sending", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const pendingDecodes: Array<(buffer: AudioBuffer) => void> = [];
    let released = false;
    const drain = () => {
      while (released && pendingDecodes.length > 0) {
        pendingDecodes.shift()?.({
          duration: 1,
          getChannelData: () => new Float32Array([0]),
          numberOfChannels: 1,
          sampleRate: 8_000,
        } as AudioBuffer);
      }
    };
    (
      window as Window & {
        __BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__?: () => void;
      }
    ).__BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__ = () => {
      released = true;
      drain();
    };
    AudioContext.prototype.decodeAudioData = () =>
      new Promise<AudioBuffer>((resolve) => {
        pendingDecodes.push(resolve);
        drain();
      });
  });
  await openGeneral(page);
  const threadId = await seedThreadRoot(page);
  await page
    .locator(
      `[data-testid="message-thread-summary"][data-thread-head-id="${threadId}"]`,
    )
    .click();
  const threadPanel = page.getByTestId("message-thread-panel");
  await expect(threadPanel).toBeVisible();

  await page.getByRole("button", { name: "Record voice note" }).first().click();
  const row = recorder(page);
  await expect(row).toHaveAttribute("data-voice-note-state", "locked");
  await page.waitForTimeout(150);
  await page.getByTestId("send-voice-note").click();
  await expect(row).toHaveAttribute("data-voice-note-state", "processing");

  // The sibling route past ProseMirror is the registry, so it has to carry
  // the same refusal the composer's own Escape does: a note the user has
  // committed to is not cancellable by an ambient key in another composer.
  await threadPanel.getByTestId("message-input").click();
  await page.keyboard.press("Escape");
  await expect(row).toHaveAttribute("data-voice-note-state", "processing");
  await expect(liveStatus(page).first()).toHaveText("Preparing voice note");
  await expect(threadPanel).toBeVisible();

  await page.evaluate(() =>
    (
      window as Window & {
        __BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__?: () => void;
      }
    ).__BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__?.(),
  );
  await expectSent(page);
});

test("Escape from a recorder control closes the mention list before it discards", async ({
  page,
}) => {
  await openGeneral(page);
  await startHandsFree(page);
  const input = page.getByTestId("message-input");
  await input.click();
  await input.pressSequentially("@al");
  const mentions = page.getByTestId("mention-autocomplete");
  await expect(mentions).toBeVisible();

  // Same key, different focus (rule 8). The row's controls are reachable by
  // Tab from the editor, and the row claims Escape in the window capture
  // phase, ahead of every editor handler that would have closed the list.
  await recorder(page).getByTestId("voice-note-pause-resume").focus();
  const stopsBefore = await mediaRecorderStops(page);
  await page.keyboard.press("Escape");
  await expect(mentions).toHaveCount(0);
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  expect(await mediaRecorderStops(page)).toBe(stopsBefore);
  await expect(liveStatus(page)).not.toHaveText("Voice note discarded");

  // With the list gone the same press discards, exactly as from the editor.
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await expect(liveStatus(page)).toHaveText("Voice note discarded");
});

test("losing the composer to navigation says the recording is gone", async ({
  page,
}) => {
  await openGeneral(page);
  const threadId = await seedThreadRoot(page);
  // Narrow enough that a thread takes the whole pane: the channel composer,
  // and the live region inside it, unmount with the recording still running.
  // The sidebar goes off canvas at this width, so the channel is opened first.
  await page.setViewportSize({ width: 560, height: 720 });
  await startHandsFree(page);
  const stopsBefore = await mediaRecorderStops(page);

  await page
    .locator(
      `[data-testid="message-thread-summary"][data-thread-head-id="${threadId}"]`,
    )
    .click();
  await expect(page.getByTestId("message-thread-panel")).toBeVisible();
  await expect(recorder(page)).toHaveCount(0);
  await expect.poll(() => mediaRecorderStops(page)).toBe(stopsBefore + 1);
  // A toast, not the live region: the live region left with the composer.
  await expect(
    toastWithText(page, "Voice note discarded when the composer closed."),
  ).toBeVisible();
  await expect(sentCards(page)).toHaveCount(0);
});

test("inside a focused thread, Escape discards the note and keeps the thread", async ({
  page,
}) => {
  await openGeneral(page);
  const threadId = await seedThreadRoot(page);
  await page
    .locator(
      `[data-testid="message-thread-summary"][data-thread-head-id="${threadId}"]`,
    )
    .click();
  const threadPanel = page.getByTestId("message-thread-panel");
  await expect(threadPanel).toBeVisible();
  const expand = page.getByRole("button", { name: "Expand thread" });
  await expand.click();
  const drawer = page.getByTestId("focus-thread-drawer-overlay");
  await expect(drawer).toBeVisible();

  // The drawer owns Escape until the recorder opens above it on the stack.
  const threadMic = threadPanel.getByRole("button", {
    name: "Record voice note",
  });
  await threadMic.click();
  const row = threadPanel.getByTestId("voice-note-recorder");
  await expect(row).toHaveAttribute("data-voice-note-state", "locked");
  await row.getByTestId("voice-note-pause-resume").focus();
  await page.keyboard.press("Escape");
  await expect(row).toHaveCount(0);
  await expect(drawer).toBeVisible();
  await expect(sentCards(page)).toHaveCount(0);

  // With the recording gone the drawer takes the key back.
  await page.keyboard.press("Escape");
  await expect(drawer).toHaveCount(0);
});

test("Escape while the note is being prepared does not discard it", async ({
  page,
}) => {
  // Hold the decode open so "processing" is a state the spec controls rather
  // than a few milliseconds it has to race.
  await page.addInitScript(() => {
    const pendingDecodes: Array<(buffer: AudioBuffer) => void> = [];
    let released = false;
    const drain = () => {
      while (released && pendingDecodes.length > 0) {
        pendingDecodes.shift()?.({
          duration: 1,
          getChannelData: () => new Float32Array([0]),
          numberOfChannels: 1,
          sampleRate: 8_000,
        } as AudioBuffer);
      }
    };
    (
      window as Window & {
        __BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__?: () => void;
      }
    ).__BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__ = () => {
      // The release can land before the decode is even queued: "processing"
      // starts when the recorder stops, and the blob arrives a tick later.
      released = true;
      drain();
    };
    AudioContext.prototype.decodeAudioData = () =>
      new Promise<AudioBuffer>((resolve) => {
        pendingDecodes.push(resolve);
        drain();
      });
  });
  await openGeneral(page);
  await startHandsFree(page);
  await page.waitForTimeout(150);
  await page.getByTestId("send-voice-note").click();
  const row = recorder(page);
  await expect(row).toHaveAttribute("data-voice-note-state", "processing");

  // Escape is ambient, and the note is already on its way out.
  await page.keyboard.press("Escape");
  await page.keyboard.press("Escape");
  await expect(row).toHaveAttribute("data-voice-note-state", "processing");
  await expect(liveStatus(page)).toHaveText("Preparing voice note");

  // The trash is still the deliberate way out, and still works here.
  await expect(row.getByTestId("voice-note-discard")).toBeEnabled();

  await page.evaluate(() =>
    (
      window as Window & {
        __BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__?: () => void;
      }
    ).__BUZZ_E2E_RESOLVE_VOICE_NOTE_DECODE__?.(),
  );
  await expectSent(page);
  await expect(liveStatus(page)).toHaveText("Voice note sent");
});

test("a cancelled press does not lock a recording", async ({ page }) => {
  await openGeneral(page);
  await pressMic(page);
  // Well inside the tap window, so a release here would lock hands free.
  await page.evaluate(() => {
    window.dispatchEvent(
      new PointerEvent("pointercancel", { bubbles: true, pointerId: 1 }),
    );
  });
  await expect(recorder(page)).toHaveCount(0);
  await page.mouse.up();
  await page.waitForTimeout(200);
  await expect(recorder(page)).toHaveCount(0);
  await expect(sentCards(page)).toHaveCount(0);
  await expect(page.getByTestId("composer-voice-note-card")).toHaveCount(0);
});
