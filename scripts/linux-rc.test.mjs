import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { test } from 'node:test';
import { checkLinuxRequest, stageLinuxCandidate } from './linux-rc.mjs';

const sha = 'a'.repeat(40);
const request = { LINUX_RC_ENABLED: 'true', REQUESTED_SHA: sha, GITHUB_SHA: sha, REPOSITORY_PRIVATE: 'true', ATTEST_PUBLIC: 'false' };
function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'mdash-linux-rc-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const put = (file, value) => {
    mkdirSync(join(root, file, '..'), { recursive: true });
    writeFileSync(join(root, file), typeof value === 'object' ? JSON.stringify(value) : value);
  };
  const version = '1.0.0-rc.1';
  put('package.json', { version });
  put('package-lock.json', { version, packages: { '': { version } } });
  put('src-tauri/tauri.conf.json', { version });
  put('src-tauri/Cargo.toml', `[package]\nname = "m-dash"\nversion = "${version}"\n`);
  put('src-tauri/Cargo.lock', `[[package]]\nname = "m-dash"\nversion = "${version}"\n`);
  return { root, put };
}
test('Linux RC defaults disabled and requires private reviewed source without attestations', t => {
  const { root } = fixture(t);
  assert.throws(() => checkLinuxRequest(root, { ...request, LINUX_RC_ENABLED: undefined }), /LINUX_RC_ENABLED/);
  assert.throws(() => checkLinuxRequest(root, { ...request, REPOSITORY_PRIVATE: 'false' }), /private/);
  assert.throws(() => checkLinuxRequest(root, { ...request, PUBLIC_ATTESTATIONS_ENABLED: 'true' }), /attestation/i);
  assert.throws(() => checkLinuxRequest(root, { ...request, ATTEST_PUBLIC: 'true' }), /attestation/i);
  assert.throws(() => checkLinuxRequest(root, { ...request, REQUESTED_SHA: 'b'.repeat(40) }), /reviewed commit/);
  assert.equal(checkLinuxRequest(root, request), '1.0.0-rc.1');
});
test('stage only Linux package and explicit provenance; reject wrong architecture and corrupt MPL source', t => {
  const { root, put } = fixture(t);
  for (const name of ['LICENSE', 'NOTICE', 'BRAND-LICENSE.txt', 'docs/USER_GUIDE.md', 'docs/PRIVACY.md', 'licenses/linux/THIRD-PARTY-NOTICES.txt']) put(name, 'fixture');
  put('licenses/linux/inventory.json', { target: 'x86_64-unknown-linux-gnu', packages: [{ name: 'selectors', version: '1.0.0', license: 'MPL-2.0', source: 'fixture' }] });
  put('src-tauri/Cargo.lock', '[[package]]\nname = "m-dash"\nversion = "1.0.0-rc.1"\n\n[[package]]\nname = "selectors"\nversion = "1.0.0"\nchecksum = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"\n');
  put('cargo/registry/cache/crates.io/selectors-1.0.0.crate', 'broken');
  put('payload/DEBIAN/control', 'Package: m-dash\nVersion: 1.0.0-rc.1\nArchitecture: arm64\nMaintainer: Fixture <fixture@example.invalid>\nDescription: Synthetic package\n');
  put('payload/usr/bin/m-dash', 'synthetic binary');
  const deb = join(root, 'src-tauri/target/x86_64-unknown-linux-gnu/release/bundle/deb/m-dash_1.0.0-rc.1_amd64.deb');
  mkdirSync(join(deb, '..'), { recursive: true });
  const build = () => execFileSync('dpkg-deb', ['--root-owner-group', '--build', join(root, 'payload'), deb], { stdio: 'pipe' });
  build();
  const env = { ...request, CARGO_HOME: join(root, 'cargo') };
  assert.throws(() => stageLinuxCandidate(root, env), /architecture/i);
  assert(!existsSync(join(root, '.local/linux-rc')));
  put('payload/DEBIAN/control', 'Package: m-dash\nVersion: 1.0.0-rc.1\nArchitecture: amd64\nMaintainer: Fixture <fixture@example.invalid>\nDescription: Synthetic package\n');
  build();
  assert.throws(() => stageLinuxCandidate(root, env), /checksum/i);
  rmSync(join(root, '.local/linux-rc'), { recursive: true, force: true });
  put('cargo/registry/cache/crates.io/selectors-1.0.0.crate', 'abc');
  put('.local/usage.sqlite', 'private fixture');
  const dir = stageLinuxCandidate(root, env);
  const sums = readFileSync(join(dir, 'SHA256SUMS'), 'utf8');
  assert(!sums.includes('usage.sqlite'));
  assert.match(sums, /sources\/selectors-1.0.0.crate/);
  for (const line of sums.trim().split('\n')) {
    const [digest, file] = line.split('  ');
    assert.equal(createHash('sha256').update(readFileSync(join(dir, file))).digest('hex'), digest);
  }
  const info = JSON.parse(readFileSync(join(dir, 'BUILDINFO.json')));
  assert.equal(info.commit, sha);
  assert.equal(info.publicAttestationRequested, false);
});
