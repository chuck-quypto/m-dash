import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, rmdirSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { delimiter, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkRequest } from './windows-rc.mjs';

const target = 'aarch64-apple-darwin';
const bundleDirectory = (root, env) => resolve(root, env.CARGO_TARGET_DIR || 'src-tauri/target', target, 'release/bundle');
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

function runStapler(action, path, { run = execFileSync, attempts = 4, pause = seconds => execFileSync('sleep', [String(seconds)]) } = {}) {
  for (let attempt = 1; ; attempt++) {
    try { return run('xcrun', ['stapler', action, path], { encoding: 'utf8', stdio: 'pipe' }); }
    catch (error) {
      const output = `${error.stdout ?? ''}\n${error.stderr ?? ''}`.trim() || error.message;
      const transient = /NSURLErrorDomain[^\n]*-(?:1001|1003|1004|1005|1006|1009)\b|network connection was lost|connection reset by peer|connection timed out/i.test(output);
      if (!transient || attempt >= attempts) throw new Error(`stapler ${action} failed for ${path}:\n${output}`, { cause: error });
      console.warn(`stapler ${action} attempt ${attempt} failed; retrying.\n${output}`);
      pause(attempt * 15);
    }
  }
}

export function buildMacCandidate(root, { env = process.env, run = execFileSync, pause } = {}) {
  const config = ['--config', 'src-tauri/tauri.macos.conf.json', '--target', target];
  const tauri = (args, commandEnv) => run('npm', ['run', 'tauri', '--', ...args], { cwd: root, env: commandEnv, stdio: 'inherit' });
  tauri(['build', ...config, '--bundles', 'app', '--ci', '--', '--locked'], env);
  const bundle = bundleDirectory(root, env);
  const app = join(bundle, 'macos/m-dash.app');
  runStapler('staple', app, { run, pause });
  runStapler('validate', app, { run, pause });

  // DMG-only bundling rebuilds and deletes macos/m-dash.app. Keep the validated original for the image and staging.
  const scratch = mkdtempSync(join(bundle, 'stapled-'));
  const stapledApp = join(scratch, 'm-dash.app');
  writeFileSync(join(scratch, 'hdiutil'), readFileSync(new URL('./mac-dmg-hdiutil.mjs', import.meta.url)), { mode: 0o700 });
  renameSync(app, stapledApp);
  try {
    const dmgEnv = { ...env, PATH: scratch + delimiter + env.PATH, MDASH_REBUILT_APP: app, MDASH_STAPLED_APP: stapledApp };
    delete dmgEnv.APPLE_API_KEY; // The disposable app needs no second notarization submission.
    tauri(['bundle', ...config, '--bundles', 'dmg', '--ci'], dmgEnv);
  } finally {
    if (existsSync(app)) renameSync(app, join(scratch, 'rebuilt.app'));
    renameSync(stapledApp, app);
    rmSync(scratch, { recursive: true });
  }
}

// Gatekeeper must accept the signed, notarized app, and the DMG must carry its own stapled ticket.
export function verifyNotarized(app, dmg, options) {
  execFileSync('codesign', ['--verify', '--deep', '--strict', app]);
  execFileSync('spctl', ['--assess', '--type', 'execute', app]);
  runStapler('validate', app, options);
  execFileSync('codesign', ['--verify', dmg]);
  execFileSync('spctl', ['--assess', '--type', 'open', '--context', 'context:primary-signature', dmg]);
  runStapler('validate', dmg, options);
}

export function stageMacCandidate(root, env, { verify = verifyNotarized } = {}) {
  const version = checkMacRequest(root, env);
  const bundle = bundleDirectory(root, env);
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
  } else if (process.argv[2] === 'build') {
    buildMacCandidate(root);
  } else {
    assert.equal(process.argv[2], 'stage', 'Use check, build or stage');
    console.log(stageMacCandidate(root, process.env));
  }
}
