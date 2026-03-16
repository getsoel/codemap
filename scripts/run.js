#!/usr/bin/env node

const { execFileSync } = require("child_process");
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

  if (platform === "darwin") return `darwin-${arch}`;
  if (platform === "win32") return "win32-x64-msvc";

  if (platform === "linux") {
    try {
      const { execSync } = require("child_process");
      const lddOutput = execSync("ldd --version 2>&1", { encoding: "utf-8" });
      if (lddOutput.toLowerCase().includes("musl") && arch === "x64") {
        return "linux-x64-musl";
      }
    } catch {}
    return `linux-${arch}-gnu`;
  }

  return null;
}

const key = detectPlatformKey();
const pkg = key ? PLATFORMS[key] : null;

if (!pkg) {
  console.error(
    `@soel/codemap: unsupported platform ${process.platform}-${process.arch}`
  );
  process.exit(1);
}

let binPath;
try {
  const pkgDir = require.resolve(`${pkg}/package.json`);
  const ext = process.platform === "win32" ? ".exe" : "";
  binPath = join(pkgDir, "..", `codemap${ext}`);
} catch {
  console.error(
    `@soel/codemap: platform package ${pkg} is not installed.\n` +
      `Try reinstalling with: npm install @soel/codemap`
  );
  process.exit(1);
}

try {
  const result = execFileSync(binPath, process.argv.slice(2), {
    stdio: "inherit",
    env: process.env,
  });
} catch (e) {
  if (e.status !== null) {
    process.exit(e.status);
  }
  throw e;
}
