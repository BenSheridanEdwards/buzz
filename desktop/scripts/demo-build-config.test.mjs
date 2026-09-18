import assert from "node:assert/strict";
import test from "node:test";

import {
  demoBuildConfig,
  productionBuildIdentity,
} from "./demo-build-config.mjs";

const expected = (name, slug) => ({
  name,
  slug,
  productName: `Buzz ${name}`,
  dmgVolumeName: `Buzz ${name}`,
  dmgFileStem: `Buzz_${name.replace(/ /g, "_")}`,
  identifier: `xyz.block.buzz.app.demo.${slug}`,
  appDataIdentity: `xyz.block.buzz.app.demo.${slug}`,
  deepLinkScheme: `buzz-demo-${slug}`,
  keyringService: `buzz-desktop-demo.${slug}`,
  nestName: `.buzz-demo-${slug}`,
  cliName: `buzz-demo-${slug}`,
  tauriConfig: {
    productName: `Buzz ${name}`,
    identifier: `xyz.block.buzz.app.demo.${slug}`,
    plugins: { "deep-link": { desktop: { schemes: [`buzz-demo-${slug}`] } } },
    bundle: { targets: ["app"] },
  },
});

test("production identity remains unchanged", () => {
  assert.deepEqual(productionBuildIdentity, {
    productName: "Buzz",
    identifier: "xyz.block.buzz.app",
    deepLinkScheme: "buzz",
    keyringService: "buzz-desktop",
    nestName: ".buzz",
    cliName: "buzz",
  });
});

test("two demo names produce complete, distinct identities", () => {
  const board = demoBuildConfig("Workstream Board", "27a4294c27a4294c");
  const interests = demoBuildConfig("Interests Demo", "deb5339adeb5339a");
  assert.deepEqual(
    board,
    expected("Workstream Board", "workstream-board-27a4294c27a4294c"),
  );
  assert.deepEqual(
    interests,
    expected("Interests Demo", "interests-demo-deb5339adeb5339a"),
  );
  for (const key of [
    "productName",
    "dmgVolumeName",
    "dmgFileStem",
    "identifier",
    "appDataIdentity",
    "deepLinkScheme",
    "keyringService",
    "nestName",
    "cliName",
  ]) {
    assert.notEqual(board[key], interests[key], key);
    assert.notEqual(board[key], productionBuildIdentity[key], key);
  }
});

test("normalized spelling aliases retain distinct runtime identities", () => {
  for (const [leftName, rightName] of [
    ["A B", "A-B"],
    ["Demo", "demo"],
    ["Workstream Board", "WORKSTREAM BOARD"],
  ]) {
    const left = demoBuildConfig(leftName, "1111111111111111");
    const right = demoBuildConfig(rightName, "2222222222222222");
    assert.notEqual(left.slug, right.slug);
    for (const key of [
      "identifier",
      "appDataIdentity",
      "deepLinkScheme",
      "keyringService",
      "nestName",
      "cliName",
    ]) {
      assert.notEqual(
        left[key],
        right[key],
        `${leftName}/${rightName}: ${key}`,
      );
    }
  }
});

test("the same display name gets a distinct identity for each build", () => {
  const first = demoBuildConfig("Demo", "1111111111111111");
  const second = demoBuildConfig("Demo", "2222222222222222");
  assert.equal(first.productName, second.productName);
  assert.equal(first.dmgFileStem, second.dmgFileStem);
  for (const key of [
    "slug",
    "identifier",
    "appDataIdentity",
    "deepLinkScheme",
    "keyringService",
    "nestName",
    "cliName",
  ]) {
    assert.notEqual(first[key], second[key], key);
  }
});

test("whitespace normalization preserves deterministic identity", () => {
  assert.deepEqual(
    demoBuildConfig("  Workstream   Board  ", "27a4294c27a4294c"),
    demoBuildConfig("Workstream Board", "27a4294c27a4294c"),
  );
});

test("maximum-length name produces a Rust-valid 48-byte slug", () => {
  const config = demoBuildConfig("x".repeat(31), "1234567812345678");
  assert.equal(config.slug.length, 48);
  assert.match(config.slug, /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/);
});

for (const name of [
  "",
  "   ",
  "Workstream/Board",
  "Workstream_Board",
  "équipe",
  "x".repeat(32),
]) {
  test(`rejects unusable name ${JSON.stringify(name)}`, () =>
    assert.throws(() => demoBuildConfig(name, "1234567812345678")));
}

test("a product name override renames the app but keeps every runtime identity", () => {
  const plain = demoBuildConfig("Fleet", "51094c33a0d51d7c");
  const renamed = demoBuildConfig("Fleet", "51094c33a0d51d7c", {
    productName: "Fleet Buzz",
  });
  assert.equal(plain.productName, "Buzz Fleet");
  assert.equal(renamed.productName, "Fleet Buzz");
  assert.equal(renamed.dmgVolumeName, "Fleet Buzz");
  assert.equal(renamed.dmgFileStem, "Fleet_Buzz");
  assert.equal(renamed.tauriConfig.productName, "Fleet Buzz");
  for (const key of [
    "slug",
    "identifier",
    "appDataIdentity",
    "deepLinkScheme",
    "keyringService",
    "nestName",
    "cliName",
  ]) {
    assert.equal(renamed[key], plain[key], key);
  }
  assert.equal(renamed.tauriConfig.identifier, plain.tauriConfig.identifier);
});

test("an empty product name override means no override", () => {
  const config = demoBuildConfig("Fleet", "51094c33a0d51d7c", {
    productName: "",
  });
  assert.equal(config.productName, "Buzz Fleet");
});

test("an icon directory swaps the bundle icon set and nothing else", () => {
  const plain = demoBuildConfig("Fleet", "51094c33a0d51d7c");
  const iconed = demoBuildConfig("Fleet", "51094c33a0d51d7c", {
    iconDir: "icons-fleet/",
  });
  assert.equal(plain.tauriConfig.bundle.icon, undefined);
  assert.deepEqual(iconed.tauriConfig.bundle.icon, [
    "icons-fleet/32x32.png",
    "icons-fleet/128x128.png",
    "icons-fleet/128x128@2x.png",
    "icons-fleet/icon.icns",
    "icons-fleet/icon.ico",
  ]);
  assert.deepEqual(iconed.tauriConfig.bundle.targets, ["app"]);
  assert.equal(iconed.identifier, plain.identifier);
});

for (const bad of ["   ", "Fleet/Buzz", "Fleet_Buzz", "équipe", "-Fleet"]) {
  test(`rejects unusable product name ${JSON.stringify(bad)}`, () =>
    assert.throws(() =>
      demoBuildConfig("Fleet", "51094c33a0d51d7c", { productName: bad }),
    ));
}

for (const bad of ["../icons", "/abs/icons", "icons/../../x"]) {
  test(`rejects unusable icon directory ${JSON.stringify(bad)}`, () =>
    assert.throws(() =>
      demoBuildConfig("Fleet", "51094c33a0d51d7c", { iconDir: bad }),
    ));
}
