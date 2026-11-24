import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";
import { verifySdk } from "./produce-required-tests.mjs";

const sdkHash = bytes => createHash('sha256').update(bytes).digest('hex');
function sdkArchive(members) {
  const blocks = [];
  for (const {path, bytes = Buffer.alloc(0), type = '0'} of members) {
    const header = Buffer.alloc(512);
    header.write(path, 0, 100);
    header.write('0000644\0', 100);
    header.write('0000000\0', 108); header.write('0000000\0', 116);
    header.write(bytes.length.toString(8).padStart(11, '0') + '\0', 124);
    header.write('00000000000\0', 136);
    header.fill(32, 148, 156); header.write(type, 156);
    header.write('ustar\0', 257); header.write('00', 263);
    const checksum = header.reduce((sum, byte) => sum + byte, 0);
    header.write(checksum.toString(8).padStart(6, '0') + '\0 ', 148);
    blocks.push(header, bytes, Buffer.alloc((512 - bytes.length % 512) % 512));
  }
  return gzipSync(Buffer.concat([...blocks, Buffer.alloc(1024)]));
}
const sdkPayload = Buffer.from('synthetic native archive payload\n');
const sdkManifest = {
  target: 'x86_64-unknown-linux-gnu', sdk_version: '1.0.2-dev.20', abi_version: 0x10002,
  inference_thread_contract: 1, association_contract: 1, models_bundled: false,
  redistribution: 'permitted-under-included-license',
  files: [{path: 'lib/core.a', sha256: sdkHash(sdkPayload)}],
};
function sdkFixture(changeManifest = () => {}, changeMembers = () => {}) {
  const manifest = structuredClone(sdkManifest);
  changeManifest(manifest);
  const manifestBytes = Buffer.from(JSON.stringify(manifest));
  const members = [{path: 'manifest.json', bytes: manifestBytes}, {path: 'lib/', type: '5'},
    {path: 'lib/core.a', bytes: sdkPayload}];
  changeMembers(members);
  const archive = sdkArchive(members);
  const entry = {sdk_version: sdkManifest.sdk_version, manifest_sha256: sdkHash(manifestBytes), archive_sha256: sdkHash(archive)};
  const native = {version: sdkManifest.sdk_version, manifest_sha256: entry.manifest_sha256, archive_sha256: entry.archive_sha256};
  return {archive, manifestBytes, entry, native};
}
const verifyFixtureSdk = f => verifySdk(f.archive, f.manifestBytes, f.entry, f.native);

export { sdkFixture, sdkHash, sdkManifest, sdkPayload, verifyFixtureSdk };
