import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import assert from 'node:assert/strict';

// Include build dependencies conservatively; none of these upstream packages is modified.
const root = resolve(import.meta.dirname, '..');
const target = process.argv[2] ?? 'x86_64-pc-windows-msvc';
assert(['x86_64-pc-windows-msvc', 'aarch64-apple-darwin', 'x86_64-unknown-linux-gnu'].includes(target), 'Qualify notices for this target first');
const mac = target === 'aarch64-apple-darwin';
const linux = target === 'x86_64-unknown-linux-gnu';
const output = mac ? join(root, 'licenses/macos') : linux ? join(root, 'licenses/linux') : root;
mkdirSync(output, { recursive: true });
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--manifest-path', join(root, 'src-tauri/Cargo.toml'), '--format-version', '1', '--locked', '--offline', '--filter-platform', target], { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 }));
const nodes = new Map(metadata.resolve.nodes.map(node => [node.id, node]));
const selected = new Set();
function visit(id) {
  if (selected.has(id)) return;
  selected.add(id);
  for (const dep of nodes.get(id)?.deps ?? []) if (dep.dep_kinds.some(kind => kind.kind !== 'dev')) visit(dep.pkg);
}
visit(metadata.resolve.root);
// The local sync crate is application source, covered by the root license.
const packages = metadata.packages.filter(pkg => selected.has(pkg.id) && pkg.id !== metadata.resolve.root && pkg.manifest_path !== join(root, 'sync-core/Cargo.toml')).sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`));
const supplemental = JSON.parse(readFileSync(join(root, 'licenses/supplemental.json'), 'utf8'))
  .filter(item => !(mac || linux) || item.packages);
if (mac) {
  supplemental.push(...JSON.parse(readFileSync(join(root, 'licenses/macos/supplemental.json'), 'utf8')));
  const rust = execFileSync('rustc', ['--version'], { encoding: 'utf8' }).split(' ')[1];
  assert.equal(rust, '1.94.1', 'Refresh Mac Rust notices for the actual compiler before building');
  supplemental.push({ name: 'Rust standard library 1.94.1', file: 'macos/Rust-standard-library-COPYRIGHT.txt', source: 'https://github.com/rust-lang/rust/tree/1.94.1/library' });
}
let compiler;
if (linux) {
  assert.equal(process.platform, 'linux', 'Generate Linux notices on the packaging host');
  compiler = execFileSync('rustc', ['--version'], { encoding: 'utf8' }).trim();
  assert.equal(compiler.split(' ')[1], '1.94.0', 'Refresh Linux Rust notices for the actual compiler before building');
  supplemental.push({ file: 'linux/dlopen2-LICENSE.txt', packages: ['dlopen2@0.8.2', 'dlopen2_derive@0.4.3'], source: 'https://raw.githubusercontent.com/OpenByteDev/dlopen2/cc80e4a0a90d499b677fdf7743699b4b3a43a989/LICENSE' });
  supplemental.push({ file: 'linux/libappindicator-LICENSE-MIT.txt', packages: ['libappindicator-sys@0.9.0'], source: 'https://github.com/tauri-apps/libappindicator-rs/tree/eafd1e3682a1247f595410266091e9684021cb6f' });
  supplemental.push({ name: 'Rust standard library 1.94.0', file: 'linux/Rust-standard-library-COPYRIGHT.txt', source: 'https://github.com/rust-lang/rust/tree/1.94.0/library' });
  // System libraries and utilities are supplied separately by Ubuntu.
  for (const name of ['libgtk-3-0t64', 'libwebkit2gtk-4.1-0', 'libsecret-1-0', 'zenity', 'libglib2.0-bin', 'xdg-utils', 'libayatana-appindicator3-1', 'libglib2.0-0t64', 'libpango-1.0-0', 'libcairo2', 'libgdk-pixbuf-2.0-0', 'libatk1.0-0t64', 'libsoup-3.0-0', 'libjavascriptcoregtk-4.1-0']) {
    const version = execFileSync('dpkg-query', ['-W', '-f=${Version}', name], { encoding: 'utf8' }).trim();
    const file = `linux/${name}-copyright.txt`;
    writeFileSync(join(root, 'licenses', file), readFileSync(`/usr/share/doc/${name}/copyright`));
    supplemental.push({ name, version, file, source: `https://packages.ubuntu.com/search?keywords=${name}&searchon=names&suite=all&section=all` });
  }
}
const sections = [];
const inventory = [];
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
function add(label, source, files) {
  assert(files.length, `No license text found for ${label}`);
  sections.push(`\n${'='.repeat(78)}\n${label}\nSource: ${source}\n`);
  const notices = files.map(file => {
    const bytes = readFileSync(file.path);
    assert(bytes.length > 0, `Empty notice: ${file.path}`);
    sections.push(`\n--- ${file.label} ---\n${bytes.toString('utf8')}\n`);
    return { file: file.label, sha256: hash(bytes) };
  });
  return notices;
}
function noticeFiles(directory, base = directory) {
  // Directory order varies by filesystem: keep each target's packaging-host order on every host
  // (case-insensitive NTFS for Windows, byte order for Mac and Linux).
  const key = entry => (mac || linux ? entry.name : entry.name.toUpperCase());
  return readdirSync(directory, { withFileTypes: true })
    .sort((a, b) => (key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0))
    .flatMap(entry => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return noticeFiles(path, base);
    return entry.isFile() && /^(licen[cs]e|copying|notice|copyright)([._-]|$)/i.test(entry.name) && !/\.(rs|c|h|nsh)$/i.test(entry.name)
      ? [{ path, label: relative(base, path).replaceAll('\\', '/') }] : [];
  });
}
for (const pkg of packages) {
  const id = `${pkg.name}@${pkg.version}`;
  const source = `https://crates.io/api/v1/crates/${pkg.name}/${pkg.version}/download`;
  const files = noticeFiles(dirname(pkg.manifest_path));
  const extra = supplemental.find(item => item.packages?.includes(id));
  if (extra) files.push({ path: join(root, 'licenses', extra.file), label: extra.file });
  inventory.push({ name: pkg.name, version: pkg.version, license: pkg.license, source, repository: pkg.repository, notices: add(`${id} (${pkg.license})`, source, files) });
}
const api = JSON.parse(readFileSync(join(root, 'node_modules/@tauri-apps/api/package.json'), 'utf8'));
inventory.push({ name: api.name, version: api.version, license: api.license, source: api.repository.url, notices: add(`${api.name}@${api.version}`, api.repository.url, noticeFiles(join(root, 'node_modules/@tauri-apps/api'))) });
for (const [name, file, source] of [
  ['Space Grotesk', 'space-grotesk', 'https://github.com/floriankarsten/space-grotesk'],
  ['Geist', 'geist', 'https://github.com/vercel/geist-font'],
  ['Geist Mono', 'geist-mono', 'https://github.com/vercel/geist-font'],
]) {
  const license = `${file}-LICENSE.txt`;
  inventory.push({ name, license: 'OFL-1.1', source, notices: add(name, source, [{ path: join(root, 'src/brand/fonts', license), label: license }]) });
}
for (const item of supplemental.filter(item => !item.packages)) {
  inventory.push({ name: item.name, ...(item.version ? { version: item.version } : {}), source: item.source, notices: add(item.name, item.source, [{ path: join(root, 'licenses', item.file), label: item.file }]) });
}
const header = `m-dash third-party notices\n\nTarget: ${target}\nGenerated with scripts/generate-notices.mjs from the locked dependency graph.\nIncludes build-time dependencies conservatively, not just code linked into the app.\nDependencies are unmodified. Their licenses remain independent of m-dash's Apache 2.0 license.\nCND assets have separate terms in BRAND-LICENSE.txt.\n\nFor each Rust crate, the Source URL below downloads its exact published source package\nwithout charge. This includes MPL-2.0 components and preserves their source-code rights.\nRelease packages must preserve this source availability and include the unmodified\nMPL component archives described in licenses/README.md.\n\n${mac ? "macOS provides WebKit. Provider CLIs are not bundled." : linux ? "Ubuntu supplies system GTK, WebKitGTK, libsecret, Zenity and the tray runtime separately.\nTheir packaging-host versions and copyright texts are recorded below; they are not bundled.\nProvider CLIs are not bundled." : "Microsoft WebView2 Runtime is installed and updated separately by Microsoft.\nThe linked WebView2 SDK loader terms are included below. Provider CLIs are not bundled."}\n`;
writeFileSync(join(output, 'THIRD-PARTY-NOTICES.txt'), header + sections.join(''));
writeFileSync(mac || linux ? join(output, 'inventory.json') : join(root, 'licenses/inventory.json'), JSON.stringify({ target, ...(compiler ? { compiler } : {}), cargoLockSha256: hash(readFileSync(join(root, 'src-tauri/Cargo.lock'))), packages: inventory }, null, 2) + '\n');
console.log(`Wrote notices for ${packages.length} Rust packages, the Tauri frontend API and ${supplemental.filter(item => !item.packages).length} supplemental components.`);
