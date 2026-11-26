import {link, lstat, mkdir, mkdtemp, open, realpath, rm, rmdir, unlink} from "node:fs/promises";
import {join} from "node:path";
import {assembleLocalRelease} from "./assemble-component-release.mjs";

// The caller supplies an exclusive runner temp directory. link() commits a fully
// closed file on the same volume and fails if the destination already exists.
export async function finalizeComponent({tag, source, batches, output, makeResult}) {
  await mkdir(output, {mode: 0o700});
  const identity = await lstat(output, {bigint: true});
  let assembled, staging, committed = false;
  async function checkOwner() {
    const current = await lstat(output, {bigint: true});
    if (!current.isDirectory() || current.dev !== identity.dev || current.ino !== identity.ino ||
        await realpath(output) !== output) throw new Error("RECEIPT_OUTPUT_IDENTITY");
  }
  try {
    assembled = await assembleLocalRelease({tag, source, batches, outputParent: output});
    const result = makeResult(assembled.receipt);
    staging = await mkdtemp(join(output, ".receipt-"));
    const file = join(staging, "receipt.json");
    const handle = await open(file, "wx", 0o600);
    try {
      await handle.writeFile(JSON.stringify(result, null, 2) + "\n");
      await handle.sync();
    } finally {
      await handle.close();
    }
    await checkOwner();
    await link(file, join(output, "component-release-receipt.json"));
    committed = true;
    return result;
  } catch {
    throw new Error("COMPONENT_RECEIPT_FAILED");
  } finally {
    // Clean only private staging names. A competing final receipt is never removed.
    await checkOwner();
    if (staging) {
      await unlink(join(staging, "receipt.json")).catch(error => {
        if (error.code !== "ENOENT") throw error;
      });
      await rmdir(staging);
    }
    if (!committed) {
      if (assembled) await rm(assembled.root, {recursive: true});
      await rmdir(output).catch(error => {
        if (!["ENOTEMPTY", "EEXIST"].includes(error.code)) throw error;
      });
    }
  }
}
