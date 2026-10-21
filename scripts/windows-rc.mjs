import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const target = 'x86_64-pc-windows-msvc';
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const read = (root, file) => readFileSync(join(root, file), 'utf8');
const json = (root, file) => JSON.parse(read(root, file));

export function checkRequest(root, env) {
  assert.match(env.REQUESTED_SHA ?? '', /^[a-f0-9]{40}$/, 'Supply the full reviewed commit SHA');
  assert.equal(env.REQUESTED_SHA, env.GITHUB_SHA, 'Workflow ref must match the reviewed commit');
  assert(['true', 'false'].includes(env.REPOSITORY_PRIVATE), 'Repository visibility is required');
  assert(['true', 'false'].includes(env.ATTEST_PUBLIC), 'Explicit attestation choice is required');
  if (env.ATTEST_PUBLIC === 'true') {
    assert.equal(env.REPOSITORY_PRIVATE, 'false', 'Attestations require a public repository and separate owner approval');
    assert.equal(env.PUBLIC_ATTESTATIONS_ENABLED, 'true', 'PUBLIC_ATTESTATIONS_ENABLED must be explicitly enabled');
  }
  const version = json(root, 'package.json').version;
  assert.match(version, /^\d+\.\d+\.\d+(-rc\.\d+)?$/, 'Use a release or release candidate version, e.g. 1.0.0 or 1.0.0-rc.1');
  const lock = json(root, 'package-lock.json');
  const versions = [lock.version, lock.packages[''].version, json(root, 'src-tauri/tauri.conf.json').version,
    read(root, 'src-tauri/Cargo.toml').match(/^version = "([^"]+)"$/m)?.[1],
    read(root, 'src-tauri/Cargo.lock').match(/\[\[package\]\]\r?\nname = "m-dash"\r?\nversion = "([^"]+)"/)?.[1]];
  for (const actual of versions) assert.equal(actual, version, 'Desktop release version mismatch');
  return version;
}

export function stageCandidate(root, env) {
  const version = checkRequest(root, env);
  const directory = join(root, '.local', 'windows-rc');
  assert(!existsSync(directory), 'Candidate staging directory already exists');
  const installer = `m-dash_${version}_x64-setup.exe`;
  const source = join(root, 'src-tauri', 'target', target, 'release', 'bundle', 'nsis', installer);
  assert(readFileSync(source).length > 0, 'Missing Windows installer');
  mkdirSync(directory, { recursive: true });
  copyFileSync(source, join(directory, installer));
  for (const file of ['LICENSE', 'NOTICE', 'BRAND-LICENSE.txt', 'THIRD-PARTY-NOTICES.txt']) {
    copyFileSync(join(root, file), join(directory, file));
  }
  for (const file of ['USER_GUIDE.md', 'PRIVACY.md']) copyFileSync(join(root, 'docs', file), join(directory, file));
  copyFileSync(join(root, 'licenses/inventory.json'), join(directory, 'inventory.json'));
  const sources = [];
  for (const pkg of json(root, 'licenses/inventory.json').packages.filter(pkg => pkg.license?.includes('MPL-2.0'))) {
    const name = `${pkg.name}-${pkg.version}.crate`;
    const cache = join(env.CARGO_HOME || join(homedir(), '.cargo'), 'registry', 'cache');
    const archive = readdirSync(cache).map(registry => join(cache, registry, name)).find(file => existsSync(file));
    assert(archive, `Missing locked source archive: ${name}`);
    const bytes = readFileSync(archive);
    const entry = read(root, 'src-tauri/Cargo.lock').split('[[package]]').find(block =>
      block.includes(`\nname = "${pkg.name}"\n`) && block.includes(`\nversion = "${pkg.version}"\n`));
    const checksum = entry?.match(/checksum = "([a-f0-9]{64})"/)?.[1];
    assert.equal(hash(bytes), checksum, `Source archive checksum mismatch: ${name}`);
    mkdirSync(join(directory, 'sources'), { recursive: true });
    copyFileSync(archive, join(directory, 'sources', name));
    sources.push({ file: `sources/${name}`, sha256: checksum, source: pkg.source });
  }
  writeFileSync(join(directory, 'SOURCE-ARCHIVES.json'), JSON.stringify(sources, null, 2) + '\n');
  const info = {
    version, target, commit: env.GITHUB_SHA, repository: env.GITHUB_REPOSITORY,
    run: env.GITHUB_RUN_ID, attempt: env.GITHUB_RUN_ATTEMPT,
    node: process.version, rust: env.RC_RUST_VERSION, cargo: env.RC_CARGO_VERSION,
    runnerImage: env.ImageOS, runnerImageVersion: env.ImageVersion,
    npmLockSha256: hash(readFileSync(join(root, 'package-lock.json'))),
    cargoLockSha256: hash(readFileSync(join(root, 'src-tauri/Cargo.lock'))),
    authenticode: false, publicAttestationRequested: env.ATTEST_PUBLIC === 'true',
  };
  writeFileSync(join(directory, 'BUILDINFO.json'), JSON.stringify(info, null, 2) + '\n');
  const files = readdirSync(directory, { recursive: true, withFileTypes: true }).filter(entry => entry.isFile())
    .map(entry => join(entry.parentPath, entry.name).slice(directory.length + 1).replaceAll('\\', '/')).sort();
  const sums = files.map(file => `${hash(readFileSync(join(directory, file)))}  ${file}`).join('\n');
  writeFileSync(join(directory, 'SHA256SUMS'), sums + '\n');
  return directory;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const root = process.cwd();
  if (process.argv[2] === 'check') {
    checkRequest(root, process.env);
    const git = args => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
    assert.equal(git(['rev-parse', 'HEAD']), process.env.GITHUB_SHA, 'Checkout must match workflow commit');
    assert.equal(git(['status', '--porcelain']), '', 'Build from a clean reviewed checkout');
    console.log('PASS exact reviewed commit, synchronized RC versions and attestation privacy gates');
  } else {
    assert.equal(process.argv[2], 'stage', 'Use check or stage');
    console.log(stageCandidate(root, process.env));
  }
}
