import {spawnSync} from 'node:child_process';
import {copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync} from 'node:fs';
import {dirname, join, relative} from 'node:path';

// Preserve upstream notices verbatim. This inventory is not a legal clearance.
export function collectNotices(root, target, buildDir, output, features = []) {
  const metadata = spawnSync('cargo', ['metadata', '--manifest-path', join(root, 'Cargo.toml'),
    '--locked', '--format-version', '1', '--filter-platform', target,
    ...(features.length ? ['--features', features.join(',')] : [])], {encoding:'utf8', maxBuffer:30*1024*1024});
  if (metadata.status !== 0) throw new Error(`License inventory failed: ${metadata.stderr}`);
  const data = JSON.parse(metadata.stdout);
  const nodes = new Map(data.resolve.nodes.map(node => [node.id,node]));
  const packages = new Map(data.packages.map(pkg => [pkg.id,pkg]));
  const entry = data.packages.find(pkg => pkg.name === 'respire_core_ffi');
  const queue = [entry.id], used = new Set();
  while (queue.length) {
    const id = queue.pop(); if (used.has(id)) continue; used.add(id);
    for (const dep of nodes.get(id).deps) {
      if (dep.dep_kinds.some(kind => kind.kind !== 'dev')) queue.push(dep.pkg);
    }
  }
  const paths = [], inventory = [], missing = [];
  const add = (source, destination) => {
    mkdirSync(dirname(join(output,destination)),{recursive:true});
    copyFileSync(source,join(output,destination)); paths.push(destination);
  };
  for (const id of [...used].sort()) {
    const pkg = packages.get(id); if (!pkg.source && pkg.name !== "tokenizers") continue;
    const directory = dirname(pkg.manifest_path), notices = [];
    const visit = dir => {
      for (const file of readdirSync(dir,{withFileTypes:true})) {
        if (file.name === '.git' || file.name === 'target') continue;
        const source = join(dir,file.name);
        if (file.isDirectory()) visit(source);
        else if (/^(license|licence|copying|notice|copyright)([._-]|$)/i.test(file.name)
          || (pkg.license_file && source === join(directory,pkg.license_file))) {
          const destination = `notices/rust/${pkg.name}-${pkg.version}/${relative(directory,source).replaceAll('\\','/')}`;
          add(source,destination); notices.push(destination);
        }
      }
    };
    visit(directory);
    // These 0.33.0 crates omit the workspace licenses; use their exact VCS revision's copies.
    if (!notices.length && pkg.version === '0.33.0' && ['mlx-rs', 'mlx-macros', 'mlx-internal-macros'].includes(pkg.name)) {
      for (const license of ['LICENSE-MIT', 'LICENSE-APACHE']) {
        const destination = `notices/rust/${pkg.name}-${pkg.version}/${license}`;
        add(join(root, 'third-party', `mlx-rs-${license}.txt`), destination);
        notices.push(destination);
      }
    }
    if (!notices.length) missing.push(`${pkg.name}@${pkg.version}`);
    inventory.push({name:pkg.name,version:pkg.version,license:pkg.license,source:pkg.source,notices});
  }
  const gccRuntimeNotices = ['libstdc++-COPYING3.txt', 'libstdc++-COPYING.RUNTIME.txt'];
  if (target.includes('musl') && gccRuntimeNotices.some(file => !existsSync(join(root, 'third-party', file)))) {
    throw new Error('Static GNU C++ runtime license and GCC Runtime Library Exception are required');
  }
  for (const file of readdirSync(join(root,'third-party'))) {
    if (gccRuntimeNotices.includes(file) && !target.includes('musl')) continue;
    if (process.env.RSRS_ORT_NOTICES_DIR && file.startsWith('onnxruntime-')) continue;
    add(join(root,'third-party',file),`notices/native/${file}`);
  }
  if (process.env.RSRS_ORT_NOTICES_DIR) {
    add(join(process.env.RSRS_ORT_NOTICES_DIR,'LICENSE'),'notices/native/onnxruntime-LICENSE.txt');
    add(join(process.env.RSRS_ORT_NOTICES_DIR,'ThirdPartyNotices.txt'),'notices/native/onnxruntime-ThirdPartyNotices.txt');
  }
  const sysroot = spawnSync('rustc',['--print','sysroot'],{encoding:'utf8'});
  if (sysroot.status !== 0) throw new Error('Cannot identify standard-library notices');
  const rustCopyright = join(sysroot.stdout.trim(),'share/doc/rust/COPYRIGHT.html');
  if (!existsSync(rustCopyright)) throw new Error('Rust COPYRIGHT.html is required for SDK distribution');
  add(rustCopyright,'notices/rust/COPYRIGHT.html');
  const inventoryPath = 'notices/dependencies.json';
  writeFileSync(join(output,inventoryPath),JSON.stringify({target,packages:inventory,missing_notice_files:missing},null,2)+'\n');
  paths.push(inventoryPath);
  if (missing.length) throw new Error(`Upstream notice files missing: ${missing.join(', ')}`);
  return paths;
}
