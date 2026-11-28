#!/usr/bin/env node
import assert from 'node:assert/strict';
import { realpathSync } from 'node:fs';

// Tauri 2.12.2 passes its regenerated .app to hdiutil create -srcfolder.
export function dmgToolArgs(args, rebuiltApp, stapledApp) {
  if (args[0] !== 'create') return args;
  const index = args.indexOf('-srcfolder');
  assert(index > 0 && args[index + 1], 'Tauri DMG creation must specify its source folder');
  assert.equal(realpathSync(args[index + 1]), realpathSync(rebuiltApp), 'Unexpected Tauri DMG source folder');
  return args.toSpliced(index + 1, 1, stapledApp);
}

if (import.meta.main) {
  const args = dmgToolArgs(process.argv.slice(2), process.env.MDASH_REBUILT_APP, process.env.MDASH_STAPLED_APP);
  process.execve('/usr/bin/hdiutil', ['hdiutil', ...args], process.env);
}
