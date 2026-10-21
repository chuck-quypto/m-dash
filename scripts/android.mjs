import { spawnSync } from 'node:child_process';
import { delimiter, join, resolve } from 'node:path';

// Per-command defaults; preserve the user's SDK and global Java selection.
const root = resolve(import.meta.dirname, '..');
const env = { ...process.env };
if (!env.JAVA_HOME && process.platform === 'darwin') {
  env.JAVA_HOME = '/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home';
}
env.ANDROID_HOME ||= join(root, '.local/android-sdk');
env.NDK_HOME ||= join(env.ANDROID_HOME, 'ndk/28.2.13676358');
const pathKey = Object.keys(env).find(key => key.toLowerCase() === 'path') ?? 'PATH';
env[pathKey] = [
  ...(env.JAVA_HOME ? [join(env.JAVA_HOME, 'bin')] : []),
  join(env.ANDROID_HOME, 'platform-tools'),
  env[pathKey] ?? '',
].join(delimiter);

const result = spawnSync(process.execPath, [
  join(root, 'node_modules/@tauri-apps/cli/tauri.js'),
  'android', ...process.argv.slice(2),
], { cwd: join(root, 'mobile'), env, stdio: 'inherit' });
if (result.error) console.error(result.error.message);
process.exit(result.status ?? 1);
