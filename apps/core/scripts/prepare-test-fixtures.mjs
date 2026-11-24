// Fixed public BGE-M3 inputs for the Core native CPU test suite.
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { promisify } from "node:util";

const run = promisify(execFile);
const hf = "https://huggingface.co/Xenova/bge-m3/resolve/4de13258303883538bd53b696b452bf8099f0858";
const fixtures = [
  [
    "onnx/model_quantized.onnx", 569694530,
    "0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d",
    `${hf}/onnx/model_quantized.onnx`,
  ],
  [
    "tokenizer.json", 17082821,
    "6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790",
    `${hf}/tokenizer.json`,
  ],
  [
    "onnx/model_quantized.cpu-v1.ort", 569654128,
    "7ff152e912bc234f5c90e4012f99897a7b2ce180f016db2321304057756b64ff",
    "https://github.com/risense-ai/respire-cli/releases/download/model-bge-m3-ort-v1/model_quantized.cpu-v1.ort",
  ],
];

async function prepare(root, verifyOnly) {
  if (!verifyOnly) {
    await mkdir(root);
    await mkdir(join(root, "onnx"));
  }
  await Promise.all(fixtures.map(async ([name, size, digest, url]) => {
    const output = join(root, name);
    if (!verifyOnly) {
      await run("curl", [
        "-q", "--fail", "--silent", "--show-error", "--location",
        "--proto", "=https", "--proto-redir", "=https", "--max-redirs", "8",
        "--retry", "2", "--retry-max-time", "360",
        "--connect-timeout", "15", "--max-time", "300", "--max-filesize", String(size),
        "--output", output, url,
      ], { timeout: 600_000 });
    }

    const info = await lstat(output);
    if (!info.isFile() || info.size !== size) {
      throw new Error(`${name}: expected a regular file of ${size} bytes`);
    }
    const hash = createHash("sha256");
    for await (const chunk of createReadStream(output)) {
      hash.update(chunk);
    }
    if (hash.digest("hex") !== digest) {
      throw new Error(`${name}: SHA-256 mismatch`);
    }
    console.log(`Verified ${name}`);
  }));
}

const [root, mode] = process.argv.slice(2);
if (!root || (mode !== undefined && mode !== "--verify-only") || process.argv.length > 4) {
  console.error("Usage: node apps/core/scripts/prepare-test-fixtures.mjs <fixture-dir> [--verify-only]");
  process.exitCode = 2;
} else {
  try {
    await prepare(resolve(root), mode === "--verify-only");
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
