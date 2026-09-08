import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { waitForAnimations } from "../helpers/animations";
import { installMockBridge } from "../helpers/bridge";

const AUDIO_URL = "http://127.0.0.1:4173/sounds/ping.mp3";
const SHOTS = "test-results/voice-note-recorder";

/**
 * Recorder states from design/voice-notes/DesktopComposer.dc.html: hold the
 * mic (pointer) or Space (keyboard) to record, release sends, Esc discards,
 * L or the chip locks hands free with pause/resume, trash and a full-width
 * Send. Every path is exercised through the modality that reaches it (rule 8).
 */

function recorder(page: Page): Locator {
  return page.getByTestId("voice-note-recorder");
}

function sentCards(page: Page): Locator {
  return page.getByTestId("audio-message-attachment");
}

async function openGeneral(page: Page) {
  await page.goto("/");
  const channel = page.getByTestId("channel-general");
  await expect(channel).toBeVisible({ timeout: 10_000 });
  await channel.click();
  await expect(page.getByTestId("message-input")).toBeVisible();
}

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

async function expectSent(page: Page, count = 1) {
  await expect(recorder(page)).toHaveCount(0);
  await expect(sentCards(page)).toHaveCount(count);
  await expect(page.getByTestId("composer-voice-note-card")).toHaveCount(0);
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
  const chip = row.getByRole("button", { name: "Hold L to lock" });
  await expect(chip).toBeVisible();
  await expect(chip).toHaveAttribute("data-locked", "false");
  await expect(row.getByTestId("voice-note-pause-resume")).toHaveCount(0);
  // The send slot stays a compact Send while the hold lasts.
  const holdSend = page.getByRole("button", { name: "Send voice note" });
  await expect(holdSend).toBeEnabled();
  await expect(holdSend).not.toHaveAttribute("data-voice-note-locked", "true");
  await page.waitForTimeout(250);
  await waitForAnimations(page);
  await page.getByTestId("message-composer-toolbar").screenshot({
    path: `${SHOTS}/recording.png`,
  });

  await page.mouse.up();
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
    /[ \u00a0]/,
  );
  await page.keyboard.press("ControlOrMeta+A");
  await page.keyboard.press("Backspace");

  await page.keyboard.down("Space");
  await expect(recorder(page)).toBeVisible();
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "recording",
  );
  // Held Space auto-repeats; none of it reaches the editor as text.
  await page.waitForTimeout(300);
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

test("Escape discards a held recording without sending", async ({ page }) => {
  await openGeneral(page);
  await pressMic(page);
  await page.keyboard.press("Escape");
  await expect(recorder(page)).toHaveCount(0);
  await page.mouse.up();
  await page.waitForTimeout(200);
  await expect(sentCards(page)).toHaveCount(0);
  await expect(page.getByTestId("composer-voice-note-card")).toHaveCount(0);

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

test("L locks a hold; locked rows pause, resume, trash and Send", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.setViewportSize({ width: 1280, height: 720 });
  await openGeneral(page);

  await pressMic(page);
  await page.keyboard.press("l");
  const row = recorder(page);
  await expect(row).toHaveAttribute("data-voice-note-state", "locked");
  const chip = row.getByTestId("voice-note-lock-chip");
  await expect(chip).toHaveAttribute("data-locked", "true");
  await expect(chip).toHaveText(/Locked/);
  // Releasing the pointer after the lock keeps recording.
  await page.mouse.up();
  await expect(row).toHaveAttribute("data-voice-note-state", "locked");
  const send = page.getByRole("button", { name: "Send voice note" });
  await expect(send).toHaveAttribute("data-voice-note-locked", "true");
  await expect(send).toHaveText("Send");
  await expect(row.getByTestId("voice-note-live-dot")).toHaveCount(0);
  await expect(
    row.getByRole("button", { name: "Discard voice note" }),
  ).toBeVisible();
  await page.waitForTimeout(250);
  await waitForAnimations(page);
  await page.getByTestId("message-composer-toolbar").screenshot({
    path: `${SHOTS}/locked.png`,
  });

  // Pause stops the clock and the waveform; resume continues the same note.
  const pause = row.getByRole("button", { name: "Pause recording" });
  await pause.click();
  await expect(row).toHaveAttribute("data-voice-note-state", "paused");
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

  // Trash discards the locked note.
  await row.getByRole("button", { name: "Discard voice note" }).click();
  await expect(recorder(page)).toHaveCount(0);
  await expect(sentCards(page)).toHaveCount(0);

  // The chip locks a keyboard hold; Enter on the locked Send sends it.
  await page.getByTestId("message-input").click();
  await page.keyboard.down("Space");
  await expect(recorder(page)).toBeVisible();
  await recorder(page).getByRole("button", { name: "Hold L to lock" }).click();
  await page.keyboard.up("Space");
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await page.waitForTimeout(150);
  await send.focus();
  await page.keyboard.press("Enter");
  await expectSent(page);

  // Enter on the focused mic (no hold to release) starts hands free.
  await page.getByRole("button", { name: "Record voice note" }).focus();
  await page.keyboard.press("Enter");
  await expect(recorder(page)).toHaveAttribute(
    "data-voice-note-state",
    "locked",
  );
  await page.waitForTimeout(150);
  await send.click();
  await expectSent(page, 2);
});

test("losing window focus releases a hold like a pointer release", async ({
  page,
}) => {
  await openGeneral(page);
  await pressMic(page);
  await page.waitForTimeout(150);
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await expectSent(page);
  await page.mouse.up();

  // A locked note is hands free: blur leaves it running.
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
});

test("the review setting holds the note in the composer with listen back, re-record and send", async ({
  page,
}) => {
  await page.addInitScript(() => {
    window.localStorage.setItem("buzz.voiceNote.reviewBeforeSend", "on");
  });
  await openGeneral(page);

  await pressMic(page);
  await page.waitForTimeout(150);
  await page.mouse.up();
  const composerCard = page.getByTestId("composer-voice-note-card");
  await expect(composerCard).toBeVisible();
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
