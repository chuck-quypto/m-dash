import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const read = file => readFileSync(join(root, file), 'utf8');
const json = file => JSON.parse(read(file));
assert(read('LICENSE').includes('Apache License'));
assert(!read('src/brand/README.md').includes('no public license selected'));
for (const file of ['THIRD-PARTY-NOTICES.txt', 'licenses/macos/THIRD-PARTY-NOTICES.txt', 'licenses/linux/THIRD-PARTY-NOTICES.txt']) {
  assert(read(file).includes("m-dash's Apache 2.0 license"), `${file} license header`);
}
const rustls = read('src-tauri/Cargo.lock').match(/name = "rustls"\r?\nversion = "([^"]+)"/)[1];
for (const file of ['licenses/inventory.json', 'licenses/macos/inventory.json', 'licenses/linux/inventory.json']) {
  assert.equal(json(file).cargoLockSha256, createHash('sha256').update(read('src-tauri/Cargo.lock').replaceAll('\r\n', '\n')).digest('hex'), `${file}: canonical desktop lockfile hash`);
  assert.equal(json(file).packages.find(pkg => pkg.name === 'rustls').version, rustls, `${file} TLS notice version`);
  for (const pkg of json(file).packages) {
    for (const notice of pkg.notices) {
      const input = [`licenses/${notice.file}`, `src/brand/fonts/${notice.file}`, ...(pkg.name === 'glib' ? [`vendor/glib/${notice.file}`] : [])].find(path => existsSync(join(root, path)));
      if (!input) continue;
      const canonical = read(input).replaceAll('\r\n', '\n');
      assert.equal(notice.sha256, createHash('sha256').update(canonical).digest('hex'), `${file}: canonical notice hash for ${input}`);
    }
  }
}
for (const file of ['NOTICE', 'BRAND-LICENSE.txt', 'docs/assets/readme-header.png']) {
  assert(readFileSync(join(root, file)).length > 0, `Missing ${file}`);
}
for (const file of ['package.json', 'mobile/package.json', 'relay/package.json']) {
  assert.equal(json(file).license, 'Apache-2.0', `${file} license`);
  assert.equal(json(file).private, true, `${file} must not publish to npm`);
}
for (const file of ['package-lock.json', 'relay/package-lock.json']) {
  assert.equal(json(file).packages[''].license, 'Apache-2.0', `${file} license`);
}
for (const file of ['src-tauri/Cargo.toml', 'sync-core/Cargo.toml', 'mobile/src-tauri/Cargo.toml']) {
  assert.match(read(file), /^license = "Apache-2.0"$/m, `${file} license`);
}
const resources = json('src-tauri/tauri.conf.json').bundle.resources;
for (const file of ['LICENSE', 'NOTICE', 'BRAND-LICENSE.txt']) {
  assert.equal(resources[`../${file}`], file, `Missing installer license ${file}`);
}
const walk = path => readdirSync(join(root, path), { withFileTypes: true }).flatMap(entry => {
  const file = join(path, entry.name);
  return entry.isDirectory() ? walk(file) : [file];
});
const tracked = execFileSync('git', ['ls-files', '-z'], { cwd: root, encoding: 'utf8' }).split('\0');
// Vendored Rust READMEs use rustdoc links rather than project file paths.
const docs = [...new Set([...tracked, ...walk('docs'), 'CONTRIBUTING.md', 'SECURITY.md'])]
  .filter(file => file.endsWith('.md') && !file.startsWith('licenses/') && !file.startsWith('vendor/') && existsSync(join(root, file)));
const failures = [];
for (const file of docs) {
  const text = read(file);
  for (const match of text.matchAll(/!?\[[^\]]*\]\(([^)]+)\)/g)) {
    const target = match[1].replace(/^<|>$/g, '').split('#')[0];
    if (!target || /^[a-z]+:/i.test(target)) continue;
    const path = resolve(root, dirname(file), decodeURIComponent(target));
    if (!existsSync(path)) failures.push(`${file}: missing ${target}`);
  }
}
assert.deepEqual(failures, [], 'Broken documentation links');
console.log(`PASS release license metadata, bundled permissions, brand header, and links in ${docs.length} Markdown files`);
