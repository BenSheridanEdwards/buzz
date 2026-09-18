import { randomBytes } from "node:crypto";
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const PRODUCTION_IDENTIFIER = "xyz.block.buzz.app";
// The build ID suffix is 17 characters including its separator, and the Rust
// build contract caps the complete demo slug at 48 ASCII bytes.
const MAX_DEMO_SLUG_LENGTH = 48;
const DEMO_BUILD_ID_SUFFIX_LENGTH = 17;
const MAX_DEMO_NAME_LENGTH = MAX_DEMO_SLUG_LENGTH - DEMO_BUILD_ID_SUFFIX_LENGTH;

export const productionBuildIdentity = Object.freeze({
  productName: "Buzz",
  identifier: PRODUCTION_IDENTIFIER,
  deepLinkScheme: "buzz",
  keyringService: "buzz-desktop",
  nestName: ".buzz",
  cliName: "buzz",
});

const DEMO_ICON_FILES = Object.freeze([
  "32x32.png",
  "128x128.png",
  "128x128@2x.png",
  "icon.icns",
  "icon.ico",
]);

function normalizeName(raw, label, maxLength) {
  if (typeof raw !== "string") throw new Error(`${label} must be text`);
  const name = raw.trim().replace(/\s+/g, " ");
  if (!name) throw new Error(`${label} must not be empty`);
  if (maxLength && name.length > maxLength) {
    throw new Error(`${label} must be at most ${maxLength} characters`);
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9 -]*$/.test(name)) {
    throw new Error(
      `${label} may contain ASCII letters, numbers, spaces, and hyphens only`,
    );
  }
  return name;
}

/**
 * Build the identity for a named demo build.
 *
 * `name` and `buildId` together fix every runtime identity (bundle identifier,
 * config home, keyring service, deep-link scheme, nest directory). Pass the
 * same `buildId` again to rebuild a demo that keeps its existing data.
 *
 * `options.productName` overrides the human-facing app name only; it never
 * touches the slug, so an installed demo can be renamed without losing its
 * agents or credentials. `options.iconDir` (relative to `src-tauri`) swaps the
 * bundle icon set for the standard Tauri file names inside that directory.
 */
export function demoBuildConfig(
  rawName,
  buildId = randomBytes(8).toString("hex"),
  options = {},
) {
  const name = normalizeName(rawName, "Demo name", MAX_DEMO_NAME_LENGTH);

  if (!/^[a-f0-9]{16}$/.test(buildId)) {
    throw new Error(
      "Demo build ID must be sixteen lowercase hexadecimal characters",
    );
  }

  const readableSlug = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  const slug = `${readableSlug}-${buildId}`;
  const productName = options.productName
    ? normalizeName(options.productName, "Demo product name")
    : `Buzz ${name}`;
  const bundle = { targets: ["app"] };
  if (options.iconDir) {
    const iconDir = String(options.iconDir).replace(/\/+$/, "");
    if (
      !/^[A-Za-z0-9][A-Za-z0-9_./-]*$/.test(iconDir) ||
      iconDir.includes("..")
    ) {
      throw new Error(
        "Demo icon directory must be a relative path under src-tauri",
      );
    }
    bundle.icon = DEMO_ICON_FILES.map((file) => `${iconDir}/${file}`);
  }
  return {
    name,
    slug,
    productName,
    dmgVolumeName: productName,
    dmgFileStem: productName.replace(/ /g, "_"),
    identifier: `${PRODUCTION_IDENTIFIER}.demo.${slug}`,
    appDataIdentity: `${PRODUCTION_IDENTIFIER}.demo.${slug}`,
    deepLinkScheme: `buzz-demo-${slug}`,
    keyringService: `buzz-desktop-demo.${slug}`,
    nestName: `.buzz-demo-${slug}`,
    cliName: `buzz-demo-${slug}`,
    tauriConfig: {
      productName,
      identifier: `${PRODUCTION_IDENTIFIER}.demo.${slug}`,
      plugins: { "deep-link": { desktop: { schemes: [`buzz-demo-${slug}`] } } },
      bundle,
    },
  };
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const [name, outputPath, buildId, productName, iconDir] =
    process.argv.slice(2);
  if (!outputPath) {
    console.error(
      "Usage: demo-build-config.mjs <demo-name> <output-config-path> [build-id] [product-name] [icon-dir]",
    );
    process.exit(2);
  }
  try {
    const config = demoBuildConfig(name, buildId || undefined, {
      productName: productName || undefined,
      iconDir: iconDir || undefined,
    });
    writeFileSync(
      outputPath,
      `${JSON.stringify(config.tauriConfig, null, 2)}\n`,
    );
    console.log(JSON.stringify(config));
  } catch (error) {
    console.error(`Invalid demo build: ${error.message}`);
    process.exit(1);
  }
}
