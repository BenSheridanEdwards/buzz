import assert from "node:assert/strict";
import test from "node:test";

import {
  acquireEscapeSurface,
  hasActiveEscapeSurface,
} from "./escapeSurfaces.ts";

test("no surfaces means background shortcuts may act", () => {
  assert.equal(hasActiveEscapeSurface(), false);
});

test("acquire and release track open surfaces", () => {
  const surfaceA = acquireEscapeSurface();
  assert.equal(hasActiveEscapeSurface(), true);

  const surfaceB = acquireEscapeSurface();
  surfaceA.release();
  assert.equal(
    hasActiveEscapeSurface(),
    true,
    "one surface closing must not release the other's claim",
  );

  surfaceB.release();
  assert.equal(hasActiveEscapeSurface(), false);
});

test("double release cannot corrupt the stack", () => {
  const surfaceA = acquireEscapeSurface();
  surfaceA.release();
  surfaceA.release();
  assert.equal(hasActiveEscapeSurface(), false);

  const surfaceB = acquireEscapeSurface();
  assert.equal(
    hasActiveEscapeSurface(),
    true,
    "a leaked double-release must not mask a genuinely open surface",
  );
  surfaceB.release();
  assert.equal(hasActiveEscapeSurface(), false);
});

test("only the newest surface acts, whatever the registration order", () => {
  // A thread panel mounts, then the recorder its composer opens.
  const panel = acquireEscapeSurface();
  const recorder = acquireEscapeSurface();
  assert.equal(panel.isTopmost(), false);
  assert.equal(
    recorder.isTopmost(),
    true,
    "the surface opened last owns Escape, not the one registered first",
  );

  // The recording ends; the panel underneath takes the key back.
  recorder.release();
  assert.equal(panel.isTopmost(), true);
});

test("a surface released from the middle leaves the top untouched", () => {
  const outer = acquireEscapeSurface();
  const middle = acquireEscapeSurface();
  const inner = acquireEscapeSurface();

  middle.release();
  assert.equal(inner.isTopmost(), true);
  assert.equal(outer.isTopmost(), false);

  inner.release();
  assert.equal(outer.isTopmost(), true);
  outer.release();
});
