import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkRequest } from './windows-rc.mjs';

const target = 'x86_64-unknown-linux-gnu';
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
export function checkLinuxRequest(root, env) {
  assert.equal(env.LINUX_RC_ENABLED, 'true', 'LINUX_RC_ENABLED must be explicitly enabled by the owner');
  assert.equal(env.REPOSITORY_PRIVATE, 'true', 'Keep the Linux candidate private');
  assert.notEqual(env.PUBLIC_ATTESTATIONS_ENABLED, 'true', 'Public attestations must remain disabled');
  assert.equal(env.ATTEST_PUBLIC, 'false', 'Linux attestations are disabled');
  return checkRequest(root, env);
}
export function stageLinuxCandidate(root, env) {
  const version = checkLinuxRequest(root, env);
  const name = `m-dash_${version}_amd64.deb`;
  const source = join(root, 'src-tauri/target', target, 'release/bundle/deb', name);
  assert.equal(execFileSync('dpkg-deb', ['-f', source, 'Architecture'], { encoding: 'utf8' }).trim(), 'amd64', 'Wrong package architecture');
  assert.equal(execFileSync('dpkg-deb', ['-f', source, 'Version'], { encoding: 'utf8' }).trim(), version, 'Wrong package version');
  const directory = join(root, '.local/linux-rc');
  assert(!existsSync(directory), 'Candidate staging directory already exists');
  const read = file => readFileSync(join(root, file));
  const inventory = JSON.parse(read('licenses/linux/inventory.json'));
  assert.equal(inventory.target, target);
  mkdirSync(directory, { recursive: true });
  copyFileSync(source, join(directory, name));
  for (const file of ['LICENSE', 'NOTICE', 'BRAND-LICENSE.txt']) copyFileSync(join(root, file), join(directory, file));
  for (const file of ['USER_GUIDE.md', 'PRIVACY.md']) copyFileSync(join(root, 'docs', file), join(directory, file));
  for (const file of ['inventory.json', 'THIRD-PARTY-NOTICES.txt']) copyFileSync(join(root, 'licenses/linux', file), join(directory, file));
  const sources = [];
  for (const pkg of inventory.packages.filter(pkg => pkg.license?.includes('MPL-2.0'))) {
    const name = `${pkg.name}-${pkg.version}.crate`;
    const cache = join(env.CARGO_HOME || join(homedir(), '.cargo'), 'registry/cache');
    const archive = readdirSync(cache).map(registry => join(cache, registry, name)).find(existsSync);
    assert(archive, `Missing locked source archive: ${name}`);
    const block = read('src-tauri/Cargo.lock').toString().split('[[package]]').find(block => block.includes(`\nname = "${pkg.name}"\n`) && block.includes(`\nversion = "${pkg.version}"\n`));
    const checksum = block?.match(/checksum = "([a-f0-9]{64})"/)?.[1];
    assert.equal(hash(readFileSync(archive)), checksum, `Source archive checksum mismatch: ${name}`);
    mkdirSync(join(directory, 'sources'), { recursive: true });
    copyFileSync(archive, join(directory, 'sources', name));
    sources.push({ file: `sources/${name}`, sha256: checksum, source: pkg.source });
  }
  writeFileSync(join(directory, 'SOURCE-ARCHIVES.json'), JSON.stringify(sources, null, 2) + '\n');
  writeFileSync(join(directory, 'BUILDINFO.json'), JSON.stringify({
    version, target, commit: env.GITHUB_SHA, repository: env.GITHUB_REPOSITORY,
    run: env.GITHUB_RUN_ID, attempt: env.GITHUB_RUN_ATTEMPT,
    node: process.version, rust: env.RC_RUST_VERSION, cargo: env.RC_CARGO_VERSION,
    runnerImage: env.ImageOS, runnerImageVersion: env.ImageVersion,
    npmLockSha256: hash(read('package-lock.json')), cargoLockSha256: hash(read('src-tauri/Cargo.lock')),
    publicAttestationRequested: false,
  }, null, 2) + '\n');
  const files = readdirSync(directory, { recursive: true, withFileTypes: true }).filter(entry => entry.isFile())
    .map(entry => join(entry.parentPath, entry.name).slice(directory.length + 1)).sort();
  writeFileSync(join(directory, 'SHA256SUMS'), files.map(file => `${hash(readFileSync(join(directory, file)))}  ${file}`).join('\n') + '\n');
  return directory;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const root = process.cwd();
  if (process.argv[2] === 'check') {
    checkLinuxRequest(root, process.env);
    const git = args => execFileSync('git', args, { encoding: 'utf8' }).trim();
    assert.equal(git(['rev-parse', 'HEAD']), process.env.GITHUB_SHA, 'Checkout must match reviewed commit');
    assert.equal(git(['status', '--porcelain']), '', 'Build from a clean reviewed checkout');
    console.log('PASS private, explicitly gated Linux RC at exact reviewed source; attestations disabled');
  } else {
    assert.equal(process.argv[2], 'stage', 'Use check or stage');
    console.log(stageLinuxCandidate(root, process.env));
  }
}
