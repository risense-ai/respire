import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { findChromium, loadPlaywright, resolvePlaywright } from "./test-models-e2e.mjs";

const cwd = fileURLToPath(new URL("../", import.meta.url));
const env = { ...process.env };
function run(command, args) {
  const result = spawnSync(command, args, { cwd, env, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(`${command} ${args.join(" ")} exited with ${result.status ?? result.signal}`);
}

for (const [suite, testRunner] of [
  ["account-session-state", false],
  ["client-refresh", false],
  ["download-dialog", true],
  ["runtime-operations", true],
  ["models-page", true],
  ["settings-proxy", false],
]) {
  run(process.execPath, [...(testRunner ? ["--test"] : []), `scripts/test-${suite}.cjs`]);
}

let temporary;
const tempRoot = resolve(tmpdir());
try {
  if (!env.PLAYWRIGHT_CORE_PATH) {
    temporary = mkdtempSync(join(tempRoot, "respire-frontend-ci-"));
    const args = ["install", "--prefix", temporary, "--no-save", "--no-package-lock", "playwright-core@1.56.1"];
    if (process.platform === "win32") {
      const npmCLI = env.npm_execpath || join(process.execPath, "../node_modules/npm/bin/npm-cli.js");
      run(process.execPath, [npmCLI, ...args]);
    } else run("npm", args);
    env.PLAYWRIGHT_CORE_PATH = join(temporary, "node_modules/playwright-core");
  } else env.PLAYWRIGHT_CORE_PATH = resolve(env.PLAYWRIGHT_CORE_PATH);
  const { chromium } = loadPlaywright(env.PLAYWRIGHT_CORE_PATH);
  let executable = findChromium(chromium, env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH);
  if (!executable && env.CI) {
    const cli = join(dirname(resolvePlaywright(env.PLAYWRIGHT_CORE_PATH)), "cli.js");
    run(process.execPath, [cli, "install", "--with-deps", "chromium"]);
    executable = findChromium(chromium);
  }
  if (!executable) throw new Error("Set PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH to a Chromium browser");
  env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH = executable;
  run(process.execPath, ["scripts/test-models-e2e.mjs"]);
} finally {
  if (temporary && dirname(resolve(temporary)) === tempRoot) rmSync(temporary, { recursive: true, force: true });
}
