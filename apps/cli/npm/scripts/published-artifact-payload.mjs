import { chmodSync, createReadStream, lstatSync, mkdirSync, readFileSync, readdirSync } from "node:fs";
import { createGunzip } from "node:zlib";
import { dirname, join } from "node:path";
import { open } from "node:fs/promises";
import { hashPattern, requireValue, sha256, versionPattern } from "./published-artifact-values.mjs";

function safePayloadPath(path) {
  requireValue(typeof path === 'string' && path.length <= 1024 && !/[\\:\x00-\x1f\x7f]/.test(path)
    && path.split('/').every(p => p && p !== '.' && p !== '..' && !/[. ]$/.test(p)
      && !/^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(p)), 'Unsafe published payload path');
  return path;
}
// Extract only bounded regular files/directories. Never delegate archive paths or links to tar.
export async function extractPublishedArchive(archive, destination) {
  mkdirSync(destination, {recursive: false});
  const input = createReadStream(archive).pipe(createGunzip());
  const iterator = input[Symbol.asyncIterator](); let buffer = Buffer.alloc(0), total = 0, count = 0;
  async function take(size) {
    while (buffer.length < size) {
      const next = await iterator.next();
      requireValue(!next.done, 'Truncated published archive');
      buffer = Buffer.concat([buffer, next.value]);
    }
    const value = buffer.subarray(0, size); buffer = buffer.subarray(size); return value;
  }
  const seen = new Set(); let pending = null, zeroes = 0;
  const field = (header, start, size) => header.subarray(start, start + size).toString('utf8').split('\0')[0];
  const octal = value => { requireValue(/^[0-7]+$/.test(value.trim()), 'Invalid tar numeric field'); return parseInt(value.trim(), 8); };
  try {
    while (true) {
      const header = await take(512);
      if (header.every(byte => byte === 0)) { if (++zeroes === 2) break; continue; }
      requireValue(zeroes === 0 && ++count <= 8192, 'Invalid tar terminator or entry count');
      const checksum = [...header].reduce((sum, byte, i) => sum + (i >= 148 && i < 156 ? 32 : byte), 0);
      requireValue(checksum === octal(field(header, 148, 8)), 'Tar checksum mismatch');
      const size = octal(field(header, 124, 12)), type = String.fromCharCode(header[156] || 48);
      requireValue(Number.isSafeInteger(size) && size <= 256 * 2 ** 20 && (total += size) <= 1024 * 2 ** 20, 'Published payload exceeds extraction limit');
      if (type === 'x' || type === 'L') {
        requireValue(pending === null && size <= 65536, 'Invalid tar extended path');
        const data = await take(size);
        if (type === 'L') pending = data.toString('utf8').replace(/\0$/, '');
        else {
          let offset = 0;
          while (offset < data.length) {
            const space = data.indexOf(32, offset), length = Number(data.subarray(offset, space).toString());
            requireValue(space > offset && Number.isSafeInteger(length) && length > space - offset + 2 && offset + length <= data.length, 'Invalid PAX record');
            const record = data.subarray(space + 1, offset + length).toString('utf8');
            requireValue(record.endsWith('\n'), 'Invalid PAX terminator');
            const equal = record.indexOf('='), key = record.slice(0, equal), value = record.slice(equal + 1, -1);
            requireValue(['path','mtime','atime','ctime','uid','gid','uname','gname'].includes(key), 'Unsupported PAX attribute');
            if (key === 'path') { requireValue(pending === null, 'Duplicate PAX path'); pending = value; }
            offset += length;
          }
        }
        await take((512 - size % 512) % 512); continue;
      }
      requireValue(type === '0' || type === '5', 'Linked or special published payload refused');
      let path = pending ?? [field(header, 345, 155), field(header, 0, 100)].filter(Boolean).join('/'); pending = null;
      path = path.replace(/^\.\//, '').replace(/\/$/, '');
      if ((path === '.' || path === '') && type === '5') { requireValue(size === 0, 'Invalid archive root'); continue; }
      safePayloadPath(path);
      requireValue(!seen.has(path.toLowerCase()), 'Duplicate published archive path'); seen.add(path.toLowerCase());
      const file = join(destination, path);
      if (type === '5') { requireValue(size === 0, 'Nonempty archive directory'); mkdirSync(file, {recursive: true}); }
      else {
        mkdirSync(dirname(file), {recursive: true});
        const output = await open(file, 'wx', 0o600);
        try {
          for (let remaining = size; remaining;) {
            const chunk = await take(Math.min(remaining, 65536)); await output.writeFile(chunk); remaining -= chunk.length;
          }
        } finally { await output.close(); }
        chmodSync(file, octal(field(header, 100, 8)) & 0o111 ? 0o755 : 0o644);
      }
      await take((512 - size % 512) % 512);
    }
    requireValue(pending === null, 'Unused tar extended path');
    requireValue(buffer.every(byte => byte === 0), 'Unexpected trailing tar data');
    for await (const chunk of { [Symbol.asyncIterator]: () => iterator }) requireValue(chunk.every(byte => byte === 0), 'Unexpected trailing tar data');
  } finally { input.destroy(); }
}
function payloadFiles(directory, prefix = '') {
  return readdirSync(join(directory, prefix), {withFileTypes: true}).flatMap(entry => {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name;
    requireValue(!entry.isSymbolicLink(), 'Linked published payload');
    if (entry.isDirectory()) return payloadFiles(directory, path);
    requireValue(entry.isFile(), 'Special published payload'); return [path];
  });
}
function verifyFile(directory, file) {
  safePayloadPath(file.path);
  const path = join(directory, file.path), info = lstatSync(path);
  requireValue(info.isFile() && !info.isSymbolicLink() && (file.size === undefined || info.size === file.size)
    && hashPattern.test(file.sha256 || '') && sha256(readFileSync(path)) === file.sha256, 'Published payload hash/size mismatch: ' + file.path);
}
export function verifyCorePayload(directory, target, lock, inventory) {
  const bytes = readFileSync(join(directory, 'core-runtime.json')), core = JSON.parse(bytes);
  requireValue(core.schema_version === 1 && core.target === target && core.sdk_version === lock.sdk_version
    && core.redistribution === 'permitted-under-included-license' && Array.isArray(core.files)
    && core.files.length > 0 && core.files.length <= 4094 && lock.redistribution === 'approved'
    && hashPattern.test(lock.manifest_sha256 || '') && hashPattern.test(lock.archive_sha256 || ''), 'Published Core identity/license mismatch');
  const expected = new Map([['core-runtime.json', null]]);
  for (const file of core.files) {
    safePayloadPath(file.path);
    requireValue(!expected.has(file.path.toLowerCase()), 'Duplicate Core payload entry');
    expected.set(file.path.toLowerCase(), file); verifyFile(directory, file);
  }
  requireValue(core.files.some(file => file.path === 'core-notices/CORE-SDK-LICENSE.txt'), 'Missing Core redistribution license');
  if (inventory) {
    requireValue(inventory.length === expected.size + 1 && payloadFiles(directory).length === inventory.length, 'Runtime/Core inventory differs');
    for (const file of inventory) {
      verifyFile(directory, file);
      if (file.path === 'core-runtime.json') requireValue(sha256(bytes) === file.sha256, 'Unbound Core manifest');
      else if (expected.has(file.path.toLowerCase())) requireValue(expected.get(file.path.toLowerCase()).sha256 === file.sha256, 'Runtime/Core file binding differs');
    }
  } else requireValue(payloadFiles(directory).length === expected.size, 'Unexpected CLI bundle payload');
  return {sdk_version: core.sdk_version, sdk_manifest_expected_sha256: lock.manifest_sha256,
    sdk_archive_expected_sha256: lock.archive_sha256, core_manifest_sha256: sha256(bytes), files_verified: core.files.length};
}
function compareVersion(a, b) {
  requireValue(versionPattern.test(a || '') && versionPattern.test(b || ''), 'Invalid compatibility version');
  const parts = v => { const [base, dev] = v.split('-dev.'); return [...base.split('.').map(Number), dev === undefined ? Number.MAX_SAFE_INTEGER : Number(dev)]; };
  const x = parts(a), y = parts(b);
  for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
  return 0;
}

export { compareVersion, payloadFiles, verifyFile };
