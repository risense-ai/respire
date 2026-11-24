import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const [selection, destination] = process.argv.slice(2);
if (selection !== 'protocol' || !destination) throw new Error('Usage: verify-interface-crate.mjs protocol <new-output>');
const output = resolve(destination);
if (existsSync(output)) throw new Error('Use a fresh crate verification directory');
mkdirSync(output, { recursive: true });
const source = join(root, 'crates', 'protocol');
const manifest = readFileSync(join(source, 'Cargo.toml'), 'utf8');
const name = /^name\s*=\s*"([^"]+)"/m.exec(manifest)?.[1];
const version = /^version\s*=\s*"([^"]+)"/m.exec(manifest)?.[1];
if (!name || !version) throw new Error('Missing crate metadata');
const run = (command, args, options = {}) => {
  const result = spawnSync(command, args, { cwd: root, stdio: 'inherit', ...options });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed`);
};
const target = process.platform === 'win32' ? 'x86_64-pc-windows-msvc' : process.platform === 'linux' ? 'x86_64-unknown-linux-gnu' : null;
if (!target) throw new Error('Crate consumer verification currently runs on Windows x64 or GNU Linux x64');
const environment = { ...process.env, CARGO_TARGET_DIR: join(output, 'target') };
run('cargo', ['package', '--locked', '--manifest-path', join(source, 'Cargo.toml')], { env: environment });
const archive = join(output, 'target/package', `${name}-${version}.crate`);
const unpacked = join(output, 'unpacked');
mkdirSync(unpacked);
cpSync(archive, join(unpacked, 'package.crate'));
// Relative archive paths also work with Windows GNU tar.
run('tar', ['-xzf', 'package.crate'], { cwd: unpacked });
const directory = join(unpacked, `${name}-${version}`), inventory = [];
const collect = (directory, prefix = '') => {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isSymbolicLink()) throw new Error('Crate inventory contains a symlink');
    if (entry.isDirectory()) collect(join(directory, entry.name), `${prefix}${entry.name}/`);
    else if (entry.isFile()) inventory.push(`${prefix}${entry.name}`);
    else throw new Error('Crate inventory contains a special file');
  }
};
collect(directory);
if (inventory.some(file => /\.(a|lib|dll|so|dylib|onnx|pt|safetensors|pdb|bc|ll)$/i.test(file))) throw new Error('Crate includes a native binary or model payload');
const report = { name, version, target, sha256: createHash('sha256').update(readFileSync(archive)).digest('hex'), files: inventory.sort() };
writeFileSync(join(output, 'PACKAGE.json'), JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify(report, null, 2));
