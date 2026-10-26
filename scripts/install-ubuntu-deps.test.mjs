import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const installer = fileURLToPath(new URL('./install-ubuntu-deps.sh', import.meta.url));
function fixture(t, mode, packages = ['libwebkit2gtk-4.1-dev', 'zenity']) {
  const root = mkdtempSync(join(tmpdir(), 'mdash-ubuntu-deps-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  mkdirSync(bin);
  const put = (name, source) => {
    const path = join(bin, name);
    writeFileSync(path, source);
    chmodSync(path, 0o755);
  };
  put('sudo', '#!/usr/bin/env bash\nexec "$@"\n');
  put('timeout', `#!/usr/bin/env node
const { appendFileSync } = require('node:fs');
const { spawnSync } = require('node:child_process');
const args = process.argv.slice(2);
appendFileSync(process.env.APT_TEST_TIMEOUT_LOG, JSON.stringify(args) + '\\n');
const result = spawnSync('/usr/bin/timeout', ['--kill-after=0.2s', '1s', ...args.slice(2)], { stdio: 'inherit' });
process.exit(result.status ?? 1);
`);
  put('apt-get', `#!/usr/bin/env node
const { appendFileSync, readFileSync } = require('node:fs');
const args = process.argv.slice(2);
const source = args.find(arg => arg.startsWith('Dir::Etc::sourcelist='))?.split('=')[1];
const sources = source ? readFileSync(source, 'utf8') : 'http://azure.archive.ubuntu.com/ubuntu';
const kind = args.includes('update') ? 'update' : args.includes('--download-only') ? 'download' : 'install';
appendFileSync(process.env.APT_TEST_LOG, JSON.stringify({ kind, args, sources, frontend: process.env.DEBIAN_FRONTEND }) + '\\n');
const network = kind === 'update' || !args.includes('--no-download');
const primary = !sources.includes('security.ubuntu.com');
if (kind !== 'update' && network && process.env.APT_TEST_MODE === 'all-fail') process.exit(100);
if (kind === 'update' && primary && process.env.APT_TEST_MODE === 'update-fail') process.exit(100);
if (kind !== 'update' && network && primary && process.env.APT_TEST_MODE === 'stall') {
  setTimeout(() => process.exit(124), 3000);
} else {
  process.exit(kind === 'install' && process.env.APT_TEST_MODE === 'install-fail' ? 100 : 0);
}
`);
  const log = join(root, 'apt.jsonl');
  const timeouts = join(root, 'timeout.jsonl');
  const result = spawnSync('bash', [installer, ...packages], {
    encoding: 'utf8', timeout: 10000,
    env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, TMPDIR: root, APT_TEST_MODE: mode, APT_TEST_LOG: log, APT_TEST_TIMEOUT_LOG: timeouts },
  });
  const readLog = path => existsSync(path) ? readFileSync(path, 'utf8').trim().split('\n').map(line => JSON.parse(line)) : [];
  return { result, calls: readLog(log), timeouts: readLog(timeouts) };
}

test('a stalled package download times out and succeeds automatically through the fallback', { skip: process.platform !== 'linux' }, t => {
  const { result, calls, timeouts } = fixture(t, 'stall');
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(calls.map(call => call.kind), ['update', 'download', 'update', 'download', 'install']);
  assert(calls[0].sources.includes('https://archive.ubuntu.com/ubuntu'));
  assert(calls[2].sources.includes('https://security.ubuntu.com/ubuntu'));
  assert.match(result.stdout + result.stderr, /124/);
  assert.deepEqual(timeouts.map(args => args.slice(0, 2)), [
    ['--kill-after=10s', '90s'], ['--kill-after=10s', '180s'],
    ['--kill-after=10s', '90s'], ['--kill-after=10s', '180s'],
  ]);
  assert(calls.at(-1).args.includes('--no-download'));
});

test('healthy downloads use signed Ubuntu repositories and install the requested packages without network fetching', { skip: process.platform !== 'linux' }, t => {
  const { result, calls, timeouts } = fixture(t, 'success');
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(calls.map(call => call.kind), ['update', 'download', 'install']);
  assert(calls[0].args.includes('--error-on=any'));
  for (const call of calls) {
    assert.match(call.sources, /signed-by=\/usr\/share\/keyrings\/ubuntu-archive-keyring.gpg/);
    assert.equal(call.sources.trim().split('\n').length, 4);
    assert(call.args.includes('Dir::Etc::sourceparts=-'));
    assert(!call.sources.includes('azure.archive.ubuntu.com'));
    if (call.kind !== 'update') assert.deepEqual(call.args.slice(-2), ['libwebkit2gtk-4.1-dev', 'zenity']);
  }
  assert(calls[1].args.includes('--download-only'));
  assert(calls[2].args.includes('--no-download'));
  assert.equal(calls[2].frontend, 'noninteractive');
  assert.equal(timeouts.length, 2, 'only network operations may be killed by a deadline');
  const source = calls[0].args.find(arg => arg.startsWith('Dir::Etc::sourcelist=')).split('=')[1];
  assert(!existsSync(source), 'temporary repository metadata must be cleaned up');
});

test('failed metadata refresh switches endpoints without using stale package lists', { skip: process.platform !== 'linux' }, t => {
  const { result, calls } = fixture(t, 'update-fail');
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(calls.map(call => call.kind), ['update', 'update', 'download', 'install']);
  assert(calls[1].sources.includes('https://security.ubuntu.com/ubuntu'));
});

test('unavailable packages on both endpoints fail after bounded attempts without installation', { skip: process.platform !== 'linux' }, t => {
  const { result, calls } = fixture(t, 'all-fail');
  assert.equal(result.status, 1);
  assert.deepEqual(calls.map(call => call.kind), ['update', 'download', 'update', 'download']);
  assert.match(result.stderr, /Both Ubuntu mirrors failed/);
});

test('an installation failure is reported without restarting package installation', { skip: process.platform !== 'linux' }, t => {
  const { result, calls } = fixture(t, 'install-fail');
  assert.equal(result.status, 100);
  assert.deepEqual(calls.map(call => call.kind), ['update', 'download', 'install']);
});

test('missing package arguments fail before invoking APT', { skip: process.platform !== 'linux' }, t => {
  const { result, calls } = fixture(t, 'success', []);
  assert.equal(result.status, 2);
  assert.deepEqual(calls, []);
});

test('both Ubuntu workflow callers use the shared prerequisite installer', () => {
  for (const name of ['ci.yml', 'linux-rc.yml']) {
    const workflow = readFileSync(new URL('../.github/workflows/' + name, import.meta.url), 'utf8');
    assert.match(workflow, /bash scripts\/install-ubuntu-deps.sh libwebkit2gtk-4.1-dev/);
    assert(!workflow.includes('sudo apt-get '));
  }
});
