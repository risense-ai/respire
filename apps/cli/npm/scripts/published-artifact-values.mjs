import { createHash } from "node:crypto";
import { releaseAssetNames } from "./verify-binary.mjs";


const sha256 = value => createHash('sha256').update(value).digest('hex');
const requireValue = (condition, message) => { if (!condition) throw new Error(message); };
const ownKeys = (value, keys) => value && typeof value === 'object' && !Array.isArray(value)
  && Object.keys(value).sort().join(',') === [...keys].sort().join(',');
const hashPattern = /^[a-f0-9]{64}$/;
const versionPattern = /^\d+\.\d+\.\d+(?:-dev\.\d+)?$/;
const sourcePattern = /^[a-f0-9]{40}$/;
const positiveID = value => Number.isSafeInteger(value) && value > 0;
export function publishedAssetNames(kind, target) {
  const extension = target.includes('windows') ? '.exe' : '';
  return kind === 'cli'
    ? releaseAssetNames({artifactLayout:'cli-frontend-v2', binaryName:'rsrs'}, target).reverse()
    : [`runtime-build-${target}.json`, `rsrs-runtime-${target}${extension}`, `rsrs-runtime-${target}.tar.gz`];
}

export { hashPattern, ownKeys, positiveID, requireValue, sha256, sourcePattern, versionPattern };
