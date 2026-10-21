import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// Returns the body of the dated CHANGELOG section for a release, without its heading.
export function releaseNotes(changelog, version) {
  const sections = changelog.split(/^## /m).slice(1);
  const section = sections.find(text => text.startsWith(`[${version}]`));
  assert(section, `CHANGELOG.md has no section for ${version}`);
  assert.match(section, /^\[[^\]]+\] - \d{4}-\d{2}-\d{2}\n/, `Date the ${version} CHANGELOG heading, e.g. ## [${version}] - 2026-10-28`);
  const body = section.slice(section.indexOf('\n') + 1).replace(/^\[[^\]]+\]: .*$/gm, '').trim();
  assert(body, `CHANGELOG section for ${version} is empty`);
  return body + '\n';
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const version = process.argv[2];
  assert.match(version ?? '', /^\d+\.\d+\.\d+(-rc\.\d+)?$/, 'Usage: node scripts/release-notes.mjs <version>');
  process.stdout.write(releaseNotes(readFileSync('CHANGELOG.md', 'utf8'), version));
}
