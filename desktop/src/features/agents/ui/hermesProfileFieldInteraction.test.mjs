/**
 * Interaction tests for the Hermes profile field.
 *
 * `HermesProfileField.test.mjs` renders the field to static markup, which can
 * see `aria-disabled` but cannot see whether the control is actually inert:
 * markup has no pointer and no keyboard. These tests mount the production
 * component in JSDOM and drive the real Radix trigger on every modality that
 * can open it (pointerdown, ArrowDown, Enter and Space), plus the item click
 * and the "Edit definition" escape hatch.
 *
 * Every negative assertion is paired with a positive control on the same
 * component without the inherited pin, so "the menu did not open" can never
 * pass because the harness failed to open it.
 */

import assert from "node:assert/strict";
import { after, afterEach, before, describe, it } from "node:test";
import { JSDOM } from "jsdom";

const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://localhost",
});

let React, act, cleanup, fireEvent, render, screen, HermesProfileField;

before(async () => {
  Object.assign(globalThis, {
    document: dom.window.document,
    window: dom.window,
    self: dom.window,
    IS_REACT_ACT_ENVIRONMENT: true,
  });
  // Radix and testing-library resolve DOM constructors off `globalThis`; Node
  // ships its own incompatible `Event`/`CustomEvent`, and JSDOM nodes reject
  // events built from them. Force every DOM constructor to JSDOM's.
  for (const key of Object.getOwnPropertyNames(dom.window)) {
    if (key === "window" || key === "document" || key === "globalThis")
      continue;
    const value = dom.window[key];
    if (
      typeof value === "function" &&
      /^(HTML|SVG)|Element$|Event$|EventTarget$|^Node|^Document|Observer$/.test(
        key,
      )
    ) {
      globalThis[key] = value;
    }
  }
  globalThis.getComputedStyle = dom.window.getComputedStyle.bind(dom.window);
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: dom.window.navigator,
    writable: true,
  });
  dom.window.matchMedia = () => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
  });
  // JSDOM has no PointerEvent. Radix's trigger opens on pointerdown and reads
  // `event.button`/`event.ctrlKey`, which a bare `Event` does not carry, so
  // without this the pointer path would look inert for the wrong reason and
  // the negative assertions below would be vacuous.
  dom.window.PointerEvent = dom.window.MouseEvent;
  globalThis.PointerEvent = dom.window.MouseEvent;
  dom.window.HTMLElement.prototype.hasPointerCapture = () => false;
  dom.window.HTMLElement.prototype.setPointerCapture = () => {};
  dom.window.HTMLElement.prototype.releasePointerCapture = () => {};
  dom.window.HTMLElement.prototype.scrollIntoView = () => {};
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
  dom.window.ResizeObserver = globalThis.ResizeObserver;
  globalThis.DOMRect = dom.window.DOMRect;

  ({ default: React } = await import("react"));
  ({ act, cleanup, fireEvent, render, screen } = await import(
    "@testing-library/react"
  ));
  ({ HermesProfileField } = await import("./HermesProfileField.tsx"));
});

afterEach(() => cleanup?.());
after(() => dom.window.close());

const bond = {
  slug: "bond",
  name: "Bond",
  description: "Executor of the Fleet",
  path: "/Users/me/.hermes/profiles/bond",
  avatarDataUrl: null,
};
const sky = {
  slug: "sky",
  name: "Sky",
  description: "Chief of the Fleet",
  path: "/Users/me/.hermes/profiles/sky",
  avatarDataUrl: null,
};

/** Mount the production field; returns its trigger and the picks it reported. */
async function mountField(overrides = {}) {
  const picks = [];
  await act(async () => {
    render(
      React.createElement(HermesProfileField, {
        disabled: false,
        envVars: { HERMES_HOME: bond.path },
        onProfileChange: (profile) => picks.push(profile),
        profiles: [bond, sky],
        status: "ready",
        ...overrides,
      }),
    );
  });
  return {
    picks,
    trigger: dom.window.document.getElementById(
      overrides.id ?? "persona-hermes-profile",
    ),
  };
}

const menuItems = () =>
  Array.from(dom.window.document.querySelectorAll('[role="menuitemradio"]'));

/** The trigger's own report of whether the menu is open. */
const isOpen = (trigger) =>
  trigger.getAttribute("data-state") === "open" ||
  trigger.getAttribute("aria-expanded") === "true";

const inheritedPin = {
  isInherited: true,
  path: bond.path,
};

describe("HermesProfileField inherited pin, driven", () => {
  // The four modalities Radix's DropdownMenuTrigger opens on:
  // `onPointerDown` toggles, `onKeyDown` opens on ArrowDown and toggles on
  // Enter and Space. Each must leave the inherited pin closed.
  const openers = [
    ["pointerdown", (trigger) => fireEvent.pointerDown(trigger)],
    [
      "ArrowDown",
      (trigger) => fireEvent.keyDown(trigger, { key: "ArrowDown" }),
    ],
    ["Enter", (trigger) => fireEvent.keyDown(trigger, { key: "Enter" })],
    ["Space", (trigger) => fireEvent.keyDown(trigger, { key: " " })],
  ];

  for (const [name, open] of openers) {
    it(`stays inert on ${name} and writes no override`, async () => {
      const { picks, trigger } = await mountField({ inherited: inheritedPin });
      assert.equal(trigger.hasAttribute("disabled"), false);
      assert.equal(trigger.getAttribute("aria-disabled"), "true");

      await act(async () => {
        open(trigger);
      });

      assert.equal(
        isOpen(trigger),
        false,
        `${name} must not open a pin the definition owns`,
      );
      assert.equal(menuItems().length, 0, `${name} rendered menu items`);
      assert.deepEqual(picks, [], `${name} wrote an instance override`);
    });

    it(`opens on ${name} when the pin is the instance's own`, async () => {
      // Positive control: the same component, same harness, same event. If
      // this one fails the assertions above prove nothing.
      const { trigger } = await mountField();
      await act(async () => {
        open(trigger);
      });
      assert.equal(isOpen(trigger), true, `${name} failed to open the menu`);
      assert.equal(menuItems().length, 3, `${name} rendered the wrong menu`);
    });
  }

  it("reaches the definition through Edit definition", async () => {
    // Markup can show the button exists; only a click shows it is wired.
    let edits = 0;
    await mountField({
      inherited: { ...inheritedPin, onEditDefinition: () => (edits += 1) },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Edit definition" }));
    });
    assert.equal(edits, 1, "the only route out of the inherited pin is dead");
  });
});

describe("HermesProfileField pick, driven", () => {
  it("reports the profile behind the clicked item", async () => {
    const { picks, trigger } = await mountField();
    await act(async () => {
      fireEvent.pointerDown(trigger);
    });
    await act(async () => {
      fireEvent.click(
        menuItems().find((item) => item.textContent?.includes("Sky")),
      );
    });
    assert.equal(picks.length, 1);
    // Not just any profile: the field maps the item's value (a path) back to
    // the profile object the form seeds itself from.
    assert.equal(picks[0]?.path, sky.path);
    assert.equal(picks[0]?.name, "Sky");
    assert.equal(isOpen(trigger), false, "picking must close the menu");
  });

  it("reports null for the none option", async () => {
    const { picks, trigger } = await mountField();
    await act(async () => {
      fireEvent.pointerDown(trigger);
    });
    await act(async () => {
      fireEvent.click(
        menuItems().find((item) => item.textContent?.includes("No profile")),
      );
    });
    assert.deepEqual(picks, [null]);
  });
});
