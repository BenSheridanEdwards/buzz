import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { installMockBridge } from "../helpers/bridge";

const AUDIO_URL = "http://127.0.0.1:4173/sounds/ping.mp3";

/**
 * The `connect-src` sources the packaged app actually ships.
 *
 * The waveform reads its audio through `fetch`, which `connect-src` governs —
 * `media-src` only covers the `<audio>` element. `vite preview` serves the
 * bundle with no policy at all, so the rest of the Playwright suite exercises
 * the waveform under a permission it does not have in a signed build. Replaying
 * this one directive (parsed from `tauri.conf.json`, never restated here) puts
 * the shipped rule back in front of the production component.
 */
function shippedConnectSrc(): string {
  const configPath = path.resolve(
    path.dirname(fileURLToPath(import.meta.url)),
    "../../src-tauri/tauri.conf.json",
  );
  const csp: string = JSON.parse(readFileSync(configPath, "utf8")).app.security
    .csp;
  const directive = csp
    .split(";")
    .map((part) => part.trim())
    .find((part) => part.startsWith("connect-src "));
  if (!directive)
    throw new Error("tauri.conf.json has no connect-src directive");
  return directive;
}

async function enforceShippedConnectSrc(page: Page) {
  const policy = shippedConnectSrc();
  await page.route("**/*", async (route) => {
    if (route.request().resourceType() !== "document") {
      await route.fallback();
      return;
    }
    const response = await route.fetch();
    await route.fulfill({
      response,
      headers: {
        ...response.headers(),
        "content-security-policy": policy,
      },
    });
  });
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

test.beforeEach(async ({ page }) => {
  await enforceShippedConnectSrc(page);
  await installMockBridge(page);
});

test("decodes a received voice note under the shipped connect-src", async ({
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
      emit({
        channelName: "general",
        content: `[voice-note-csp.mp3](${audioUrl})`,
        extraTags: [
          [
            "imeta",
            `url ${audioUrl}`,
            "m audio/mpeg",
            "duration 1.2",
            "filename voice-note-csp.mp3",
          ],
        ],
      });
    },
    { audioUrl: AUDIO_URL },
  );

  const card = page.getByTestId("audio-message-attachment").last();
  const waveform = card.getByTestId("voice-note-playback-waveform");
  // Relay media arrives as bytes over IPC and is handed to `<audio>` as an
  // object URL; the waveform then reads that same object URL back with `fetch`.
  // Drop `blob:` from the shipped `connect-src` and WebKit refuses that read,
  // leaving every card on the flat dotted placeholder while playback still
  // works — the exact split this test pins.
  await expect(waveform).toHaveAttribute("data-waveform-state", "ready");
  await expect(card.getByRole("status")).toHaveCount(0);
  await expect
    .poll(() =>
      waveform
        .locator("span")
        .evaluateAll((bars) =>
          bars.some((bar) => bar.getBoundingClientRect().height > 3),
        ),
    )
    .toBe(true);
});
