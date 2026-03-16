const { existsSync } = require("fs");
const { join } = require("path");

const PLATFORMS = {
  "darwin-arm64": "@soel/codemap-darwin-arm64",
  "darwin-x64": "@soel/codemap-darwin-x64",
  "linux-x64-gnu": "@soel/codemap-linux-x64-gnu",
  "linux-arm64-gnu": "@soel/codemap-linux-arm64-gnu",
  "linux-x64-musl": "@soel/codemap-linux-x64-musl",
  "win32-x64-msvc": "@soel/codemap-win32-x64-msvc",
};

function detectPlatformKey() {
  const platform = process.platform;
  const arch = process.arch;

  if (platform === "darwin") {
    return `darwin-${arch}`;
  }

  if (platform === "win32") {
    return "win32-x64-msvc";
  }

  if (platform === "linux") {
    // Detect musl vs glibc
    const isMusl = (() => {
      try {
        // If ldd points to musl, we're on musl
        const { execSync } = require("child_process");
        const lddOutput = execSync("ldd --version 2>&1", {
          encoding: "utf-8",
        });
        return lddOutput.toLowerCase().includes("musl");
      } catch {
        return false;
      }
    })();

    if (isMusl && arch === "x64") {
      return "linux-x64-musl";
    }
    return `linux-${arch}-gnu`;
  }

  return null;
}

const key = detectPlatformKey();

if (!key) {
  console.warn(
    `@soel/codemap: unsupported platform ${process.platform}-${process.arch}`
  );
  process.exit(0);
}

const pkg = PLATFORMS[key];

if (!pkg) {
  console.warn(`@soel/codemap: no binary package for ${key}`);
  process.exit(0);
}

// Verify the platform package was installed
try {
  require.resolve(pkg);
} catch {
  console.warn(
    `@soel/codemap: platform package ${pkg} not installed. ` +
      `This may happen if your package manager doesn't install optionalDependencies.`
  );
  process.exit(0);
}
