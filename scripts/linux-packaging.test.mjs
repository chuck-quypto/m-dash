import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const read = file => readFileSync(new URL(`../${file}`, import.meta.url));
const config = JSON.parse(read('src-tauri/tauri.linux.conf.json'));
for (const dependency of ['libsecret-1-0', 'zenity', 'libglib2.0-bin', 'xdg-utils']) {
  assert(config.bundle.linux.deb.depends.includes(dependency), `Declare runtime dependency ${dependency}`);
}
const otherPlatforms = ['THIRD-PARTY-NOTICES.txt', 'licenses/inventory.json', 'licenses/macos/THIRD-PARTY-NOTICES.txt', 'licenses/macos/inventory.json'];
const before = otherPlatforms.map(read);
execFileSync(process.execPath, ['scripts/generate-notices.mjs', 'x86_64-unknown-linux-gnu'], { cwd: root, stdio: 'pipe' });
const inventory = JSON.parse(read('licenses/linux/inventory.json'));
assert.equal(inventory.target, 'x86_64-unknown-linux-gnu');
assert.match(inventory.compiler, /^rustc 1\.94\.0 /);
assert(inventory.packages.some(pkg => pkg.name === 'gtk'));
assert(inventory.packages.some(pkg => pkg.name === 'libsecret-1-0'));
assert(inventory.packages.some(pkg => pkg.name === 'libglib2.0-bin'));
assert(inventory.packages.some(pkg => pkg.name === 'xdg-utils'));
const notices = read('licenses/linux/THIRD-PARTY-NOTICES.txt').toString();
assert.match(notices, /system.*WebKitGTK/i);
assert(!/NSIS installer|Microsoft WebView2 SDK|macOS provides/.test(notices));
for (const [i, file] of otherPlatforms.entries()) assert.deepEqual(read(file), before[i], `${file} must remain unchanged`);
const generated = read('licenses/linux/THIRD-PARTY-NOTICES.txt');
execFileSync(process.execPath, ['scripts/generate-notices.mjs', 'x86_64-unknown-linux-gnu'], { cwd: root, stdio: 'pipe' });
assert.deepEqual(read('licenses/linux/THIRD-PARTY-NOTICES.txt'), generated, 'Linux notices regenerate deterministically on the packaging host');
console.log('PASS Linux graph, compiler, system notices, deterministic regeneration and other-platform preservation');
