import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmdirSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkRequest } from './windows-rc.mjs';

const target = 'aarch64-apple-darwin';
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const files = directory => readdirSync(directory, { recursive: true, withFileTypes: true }).filter(entry => entry.isFile())
  .map(entry => relative(directory, join(entry.parentPath, entry.name))).sort();

const signingSettings = ['APPLE_CERTIFICATE', 'APPLE_CERTIFICATE_PASSWORD', 'APPLE_SIGNING_IDENTITY', 'APPLE_API_ISSUER', 'APPLE_API_KEY', 'APPLE_API_KEY_PATH'];

export function checkMacRequest(root, env) {
  assert.equal(env.REPOSITORY_PRIVATE, 'false', 'Signed Mac releases are built only in the public repository');
  assert.equal(env.ATTEST_PUBLIC, 'true', 'Signed Mac releases require public attestations');
  return checkRequest(root, env);
}

export function checkMacSigning(env) {
  for (const name of signingSettings) assert(env[name], `Missing Mac signing setting: ${name}`);
  assert.match(env.APPLE_SIGNING_IDENTITY, /^Developer ID Application: /, 'Sign with a Developer ID Application certificate');
  assert(existsSync(env.APPLE_API_KEY_PATH), 'App Store Connect API key file is missing');
}

// Gatekeeper must accept the signed, notarized app, and the DMG must carry its own stapled ticket.
export function verifyNotarized(app, dmg) {
  const run = (command, args) => execFileSync(command, args, { stdio: 'pipe' });
  run('codesign', ['--verify', '--deep', '--strict', app]);
  run('spctl', ['--assess', '--type', 'execute', app]);
  run('xcrun', ['stapler', 'validate', app]);
  run('codesign', ['--verify', dmg]);
  run('spctl', ['--assess', '--type', 'open', '--context', 'context:primary-signature', dmg]);
  run('xcrun', ['stapler', 'validate', dmg]);
}

export function stageMacCandidate(root, env, { verify = verifyNotarized } = {}) {
  const version = checkMacRequest(root, env);
  const bundle = join(root, 'src-tauri/target', target, 'release/bundle');
  const app = join(bundle, 'macos/m-dash.app');
  let architectures;
  try { architectures = execFileSync('lipo', ['-archs', join(app, 'Contents/MacOS/m-dash')], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim(); }
  catch { throw new Error('Wrong package architecture: expected an arm64 Mach-O'); }
  assert.equal(architectures, 'arm64', 'Wrong package architecture');
  const plist = key => execFileSync('plutil', ['-extract', key, 'raw', '-o', '-', join(app, 'Contents/Info.plist')], { encoding: 'utf8' }).trim();
  assert.equal(plist('CFBundleShortVersionString'), version, 'Wrong bundle version');
  assert.equal(plist('CFBundleIdentifier'), 'net.chuckdudley.mdash');
  const name = `m-dash_${version}_aarch64.dmg`;
  const source = join(bundle, 'dmg', name);
  verify(app, source);
  const mount = mkdtempSync(join(tmpdir(), 'mdash-rc-dmg-'));
  let mounted = false;
  try {
    execFileSync('hdiutil', ['attach', '-readonly', '-nobrowse', '-quiet', '-mountpoint', mount, source], { stdio: 'pipe' });
    mounted = true;
    const packaged = join(mount, 'm-dash.app');
    assert.deepEqual(files(packaged), files(app), 'DMG must contain the exact built app');
    for (const file of files(app)) assert.equal(hash(readFileSync(join(packaged, file))), hash(readFileSync(join(app, file))), 'DMG must contain the exact built app');
  } finally {
    if (mounted) execFileSync('hdiutil', ['detach', '-quiet', mount], { stdio: 'pipe' });
    rmdirSync(mount);
  }
  const directory = join(root, '.local/mac-rc');
  assert(!existsSync(directory), 'Candidate staging directory already exists');
  const read = file => readFileSync(join(root, file));
  const inventory = JSON.parse(read('licenses/macos/inventory.json'));
  assert.equal(inventory.target, target);
  assert.equal(inventory.cargoLockSha256, hash(read('src-tauri/Cargo.lock')), 'Mac notices must match the locked graph');
  mkdirSync(directory, { recursive: true });
  copyFileSync(source, join(directory, name));
  for (const file of ['LICENSE', 'NOTICE', 'BRAND-LICENSE.txt']) copyFileSync(join(root, file), join(directory, file));
  for (const file of ['USER_GUIDE.md', 'PRIVACY.md']) copyFileSync(join(root, 'docs', file), join(directory, file));
  for (const file of ['inventory.json', 'THIRD-PARTY-NOTICES.txt']) copyFileSync(join(root, 'licenses/macos', file), join(directory, file));
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
    sdk: env.RC_MACOS_SDK, xcode: env.RC_XCODE_VERSION,
    runnerImage: env.ImageOS, runnerImageVersion: env.ImageVersion,
    npmLockSha256: hash(read('package-lock.json')), cargoLockSha256: hash(read('src-tauri/Cargo.lock')),
    signing: 'Developer ID; app and DMG notarized and stapled', appleSubmission: true, publicAttestationRequested: true,
  }, null, 2) + '\n');
  writeFileSync(join(directory, 'SHA256SUMS'), files(directory).map(file => `${hash(readFileSync(join(directory, file)))}  ${file}`).join('\n') + '\n');
  return directory;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const root = process.cwd();
  if (process.argv[2] === 'check') {
    checkMacRequest(root, process.env);
    checkMacSigning(process.env);
    execFileSync(process.execPath, ['scripts/windows-rc.mjs', 'check'], { cwd: root, stdio: 'inherit' });
  } else {
    assert.equal(process.argv[2], 'stage', 'Use check or stage');
    console.log(stageMacCandidate(root, process.env));
  }
}
