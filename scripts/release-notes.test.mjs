import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { releaseNotes } from './release-notes.mjs';

const changelog = `# Changelog

## [Unreleased]

## [1.1.0] - 2026-11-02

### Fixed
- Something.

## [1.0.0] - Unreleased

### Added
- First release.

[1.1.0]: https://example.com/1.1.0
`;

test('release notes are the dated section body, without link references', () => {
  assert.equal(releaseNotes(changelog, '1.1.0'), '### Fixed\n- Something.\n');
});

test('an undated or missing section stops the release', () => {
  assert.throws(() => releaseNotes(changelog, '1.0.0'), /Date the 1\.0\.0/);
  assert.throws(() => releaseNotes(changelog, '2.0.0'), /no section/);
});

test('the current version has a CHANGELOG section', () => {
  const { version } = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
  const text = readFileSync(new URL('../CHANGELOG.md', import.meta.url), 'utf8');
  assert.match(text, new RegExp(`^## \\[${version.replaceAll('.', '\\.')}\\] - `, 'm'));
});
