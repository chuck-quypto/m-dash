import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { checkRequest, stageCandidate } from './windows-rc.mjs';

const sha = 'a'.repeat(40);
const request = {
  GITHUB_SHA: sha, REQUESTED_SHA: sha, REPOSITORY_PRIVATE: 'true',
  ATTEST_PUBLIC: 'false', PUBLIC_ATTESTATIONS_ENABLED: '',
};

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'mdash-rc-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const put = (file, value) => {
    mkdirSync(join(root, file, '..'), { recursive: true });
    writeFileSync(join(root, file), typeof value === 'object' ? JSON.stringify(value) : value);
  };
  put('package.json', { version: '1.0.0-rc.1' });
  put('package-lock.json', { version: '1.0.0-rc.1', packages: { '': { version: '1.0.0-rc.1' } } });
  put('src-tauri/tauri.conf.json', { version: '1.0.0-rc.1' });
  put('src-tauri/Cargo.toml', '[package]\nname = "m-dash"\nversion = "1.0.0-rc.1"\n');
  put('src-tauri/Cargo.lock', '[[package]]\nname = "m-dash"\nversion = "1.0.0-rc.1"\n');
  return { root, put };
}

test('private candidates cannot submit public attestations; both public gates are required', t => {
  const { root } = fixture(t);
  assert.equal(checkRequest(root, request), '1.0.0-rc.1');
  assert.throws(() => checkRequest(root, { ...request, ATTEST_PUBLIC: 'true', PUBLIC_ATTESTATIONS_ENABLED: 'true' }), /public repository/);
  assert.throws(() => checkRequest(root, { ...request, REPOSITORY_PRIVATE: 'false', ATTEST_PUBLIC: 'true' }), /PUBLIC_ATTESTATIONS_ENABLED/);
  assert.equal(checkRequest(root, { ...request, REPOSITORY_PRIVATE: 'false', ATTEST_PUBLIC: 'true', PUBLIC_ATTESTATIONS_ENABLED: 'true' }), '1.0.0-rc.1');
});

test('candidate must match the reviewed commit and all source versions', t => {
  const { root, put } = fixture(t);
  assert.throws(() => checkRequest(root, { ...request, REQUESTED_SHA: 'b'.repeat(40) }), /reviewed commit/);
  assert.throws(() => checkRequest(root, { ...request, REQUESTED_SHA: 'main' }), /reviewed commit/);
  put('src-tauri/tauri.conf.json', { version: '1.0.0-rc.2' });
  assert.throws(() => checkRequest(root, request), /version mismatch/);
  put('package.json', { version: '1.0.0-beta.1' });
  assert.throws(() => checkRequest(root, request), /release candidate version/);
});

test('stage only the installer and explicit release materials, with verifiable hashes', t => {
  const { root, put } = fixture(t);
  const installer = 'm-dash_1.0.0-rc.1_x64-setup.exe';
  put(`src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/${installer}`, 'synthetic installer');
  for (const name of ['LICENSE', 'NOTICE', 'BRAND-LICENSE.txt', 'THIRD-PARTY-NOTICES.txt', 'docs/USER_GUIDE.md', 'docs/PRIVACY.md']) put(name, 'fixture');
  put('licenses/inventory.json', { packages: [] });
  put('.local/usage.sqlite', 'private data');
  const directory = stageCandidate(root, { ...request, GITHUB_REPOSITORY: 'example/m-dash', GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '1' });
  const sums = readFileSync(join(directory, 'SHA256SUMS'), 'utf8');
  assert.match(sums, new RegExp(`^[a-f0-9]{64}  ${installer.replaceAll('.', '\\.')}$`, 'm'));
  assert(!sums.includes('usage.sqlite'));
  for (const line of sums.trim().split('\n')) {
    const [expected, file] = line.split('  ');
    assert.equal(createHash('sha256').update(readFileSync(join(directory, file))).digest('hex'), expected);
  }
  const info = JSON.parse(readFileSync(join(directory, 'BUILDINFO.json'), 'utf8'));
  assert.equal(info.commit, sha);
  assert.equal(info.authenticode, false);
  assert.equal(info.publicAttestationRequested, false);
  assert.throws(() => stageCandidate(root, request), /already exists/);
});

test('release materials include exact MPL crate sources and reject a corrupted archive', t => {
  const { root, put } = fixture(t);
  put('src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/m-dash_1.0.0-rc.1_x64-setup.exe', 'installer');
  for (const name of ['LICENSE', 'NOTICE', 'BRAND-LICENSE.txt', 'THIRD-PARTY-NOTICES.txt', 'docs/USER_GUIDE.md', 'docs/PRIVACY.md']) put(name, 'fixture');
  put('licenses/inventory.json', { packages: [{ name: 'selectors', version: '1.0.0', license: 'MPL-2.0', source: 'https://crates.io/api/v1/crates/selectors/1.0.0/download' }] });
  put('src-tauri/Cargo.lock', '[[package]]\nname = "m-dash"\nversion = "1.0.0-rc.1"\n\n[[package]]\nname = "selectors"\nversion = "1.0.0"\nchecksum = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"\n');
  put('cargo/registry/cache/crates.io/selectors-1.0.0.crate', 'broken');
  const env = { ...request, CARGO_HOME: join(root, 'cargo') };
  assert.throws(() => stageCandidate(root, env), /Source archive checksum mismatch/);
  rmSync(join(root, '.local/windows-rc'), { recursive: true });
  put('cargo/registry/cache/crates.io/selectors-1.0.0.crate', 'abc');
  const directory = stageCandidate(root, env);
  assert.equal(readFileSync(join(directory, 'sources/selectors-1.0.0.crate'), 'utf8'), 'abc');
  assert.match(readFileSync(join(directory, 'SHA256SUMS'), 'utf8'), /ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  sources\/selectors-1.0.0.crate/);
});
