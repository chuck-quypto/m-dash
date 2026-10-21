import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { test } from 'node:test';
import { checkMacRequest, checkMacSigning, stageMacCandidate } from './mac-rc.mjs';

const sha = 'a'.repeat(40);
const signing = {
  APPLE_CERTIFICATE: 'synthetic', APPLE_CERTIFICATE_PASSWORD: 'synthetic',
  APPLE_SIGNING_IDENTITY: 'Developer ID Application: Example (EXAMPLE123)',
  APPLE_API_ISSUER: 'synthetic', APPLE_API_KEY: 'synthetic',
};
const base = { REQUESTED_SHA: sha, GITHUB_SHA: sha, REPOSITORY_PRIVATE: 'false', ATTEST_PUBLIC: 'true', PUBLIC_ATTESTATIONS_ENABLED: 'true' };
let request, signed;
function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'mdash-mac-rc-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const put = (file, value) => {
    mkdirSync(join(root, file, '..'), { recursive: true });
    writeFileSync(join(root, file), typeof value === 'object' ? JSON.stringify(value) : value);
  };
  put('AuthKey.p8', 'synthetic');
  request = base;
  signed = { ...signing, APPLE_API_KEY_PATH: join(root, 'AuthKey.p8') };
  const version = '1.0.0-rc.1';
  put('package.json', { version });
  put('package-lock.json', { version, packages: { '': { version } } });
  put('src-tauri/tauri.conf.json', { version });
  put('src-tauri/Cargo.toml', `[package]\nname = "m-dash"\nversion = "${version}"\n`);
  put('src-tauri/Cargo.lock', `[[package]]\nname = "m-dash"\nversion = "${version}"\n`);
  return { root, put };
}

test('Mac release requires the public repository, attestations and every signing setting', t => {
  const { root, put } = fixture(t);
  assert.throws(() => checkMacRequest(root, { ...request, REPOSITORY_PRIVATE: 'true' }), /public repository/);
  assert.throws(() => checkMacRequest(root, { ...request, ATTEST_PUBLIC: 'false' }), /attestation/i);
  assert.throws(() => checkMacRequest(root, { ...request, PUBLIC_ATTESTATIONS_ENABLED: '' }), /PUBLIC_ATTESTATIONS_ENABLED/);
  checkMacSigning(signed);
  assert.throws(() => checkMacSigning({ ...signed, APPLE_API_KEY: '' }), /APPLE_API_KEY/);
  assert.throws(() => checkMacSigning({ ...signed, APPLE_SIGNING_IDENTITY: 'Apple Development: Example' }), /Developer ID/);
  assert.throws(() => checkMacSigning({ ...signed, APPLE_API_KEY_PATH: join(root, 'missing.p8') }), /key file/);
  assert.throws(() => checkMacRequest(root, { ...request, REQUESTED_SHA: 'b'.repeat(40) }), /reviewed commit/);
  assert.equal(checkMacRequest(root, request), '1.0.0-rc.1');
  put('src-tauri/tauri.conf.json', { version: '1.0.0-rc.2' });
  assert.throws(() => checkMacRequest(root, request), /version mismatch/);
});

test('Mac staging binds the DMG to the app and includes only explicit materials and verified MPL sources', { skip: process.platform !== 'darwin' }, t => {
  const { root, put } = fixture(t);
  const target = 'aarch64-apple-darwin';
  const bundle = `src-tauri/target/${target}/release/bundle`;
  const app = `${bundle}/macos/m-dash.app/Contents`;
  put(`${app}/Info.plist`, { CFBundleIdentifier: 'net.chuckdudley.mdash', CFBundleShortVersionString: '1.0.0-rc.1' });
  put(`${app}/MacOS/m-dash`, 'not a Mach-O');
  for (const name of ['LICENSE', 'NOTICE', 'BRAND-LICENSE.txt']) put(name, 'fixture');
  for (const name of ['USER_GUIDE.md', 'PRIVACY.md']) put(`docs/${name}`, 'fixture');
  put('licenses/macos/THIRD-PARTY-NOTICES.txt', 'fixture Mac notices');
  const lock = '[[package]]\nname = "m-dash"\nversion = "1.0.0-rc.1"\n\n[[package]]\nname = "selectors"\nversion = "1.0.0"\nchecksum = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"\n';
  put('src-tauri/Cargo.lock', lock);
  put('licenses/macos/inventory.json', { target, cargoLockSha256: createHash('sha256').update(lock).digest('hex'), packages: [{ name: 'selectors', version: '1.0.0', license: 'MPL-2.0', source: 'fixture' }] });
  put('cargo/registry/cache/crates.io/selectors-1.0.0.crate', 'broken');
  const env = { ...request, CARGO_HOME: join(root, 'cargo') };
  // The synthetic app is unsigned, so record what would be verified instead of asking Gatekeeper.
  const verified = [];
  const unsigned = { verify: (...paths) => verified.push(paths) };
  assert.throws(() => stageMacCandidate(root, env, unsigned), /architecture/i);
  assert(!existsSync(join(root, '.local/mac-rc')));
  execFileSync('clang', ['-arch', 'arm64', '-c', '-x', 'c', '-', '-o', join(root, app, 'MacOS/m-dash')], { input: 'int main(void) { return 0; }' });
  const dmg = join(root, bundle, 'dmg/m-dash_1.0.0-rc.1_aarch64.dmg');
  mkdirSync(join(dmg, '..'), { recursive: true });
  execFileSync('hdiutil', ['create', '-srcfolder', join(root, bundle, 'macos'), '-volname', 'Synthetic m-dash', '-format', 'UDZO', dmg], { stdio: 'pipe' });
  put(`${app}/Resources/private.sqlite`, 'must not silently diverge from the DMG');
  assert.throws(() => stageMacCandidate(root, env, unsigned), /DMG.*app/i);
  rmSync(join(root, app, 'Resources'), { recursive: true });
  assert.throws(() => stageMacCandidate(root, env, unsigned), /checksum/i);
  rmSync(join(root, '.local/mac-rc'), { recursive: true, force: true });
  put('cargo/registry/cache/crates.io/selectors-1.0.0.crate', 'abc');
  put('.local/usage.sqlite', 'private fixture');
  const directory = stageMacCandidate(root, env, unsigned);
  const sums = readFileSync(join(directory, 'SHA256SUMS'), 'utf8');
  assert(!sums.includes('usage.sqlite'));
  assert.match(sums, /sources\/selectors-1.0.0.crate/);
  for (const line of sums.trim().split('\n')) {
    const [digest, file] = line.split('  ');
    assert.equal(createHash('sha256').update(readFileSync(join(directory, file))).digest('hex'), digest);
  }
  const info = JSON.parse(readFileSync(join(directory, 'BUILDINFO.json')));
  assert.equal(info.commit, sha);
  assert.equal(info.publicAttestationRequested, true);
  assert.equal(info.appleSubmission, true);
  assert.deepEqual(verified.at(-1), [join(root, bundle, 'macos/m-dash.app'), dmg]);
  rmSync(directory, { recursive: true });
  assert.throws(() => stageMacCandidate(root, env, { verify: () => { throw new Error('rejected by Gatekeeper'); } }), /Gatekeeper/);
  assert(!existsSync(directory));
});

test('Release workflow is manual, main-only and keeps signing secrets and write access in their own jobs', () => {
  const workflow = readFileSync(new URL('../.github/workflows/release.yml', import.meta.url), 'utf8');
  assert.match(workflow, /on:\n  workflow_dispatch:/);
  assert(!/^  (push|pull_request|schedule|release):/m.test(workflow));
  assert.match(workflow, /permissions:\n  contents: read/);
  const jobs = Object.fromEntries(workflow.split(/^  (?=[a-z-]+:\n)/m).slice(1).map(job => [job.slice(0, job.indexOf(':')), job]));
  assert.deepEqual(Object.keys(jobs), ['windows', 'mac', 'attest', 'draft']);
  for (const job of Object.values(jobs)) assert.match(job, /github\.ref == 'refs\/heads\/main' && !github\.event\.repository\.private/);
  for (const [name, job] of Object.entries(jobs)) {
    assert.equal(/secrets\.APPLE_/.test(job), name === 'mac', `${name} must not see Apple secrets`);
    assert.equal(/environment: release/.test(job), name === 'mac');
    assert.equal(/id-token: write/.test(job), name === 'attest');
    assert.equal(/contents: write/.test(job), name === 'draft');
  }
  assert(!/--no-sign/.test(workflow));
  assert.match(jobs.mac, /scripts\/mac-rc\.mjs check/);
  assert.match(jobs.mac, /scripts\/mac-rc\.mjs stage/);
  assert.match(jobs.draft, /gh release create .*--draft/);
});
