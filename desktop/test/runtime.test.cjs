const test = require('node:test');
const assert = require('node:assert/strict');
const { lineToBackendUrl, pythonCandidates, resolvePythonPath, validateHealth } = require('../lib/runtime.cjs');

test('parses only the exact local backend readiness line', () => {
  assert.equal(lineToBackendUrl('listening on http://127.0.0.1:43127'), 'http://127.0.0.1:43127');
  assert.equal(lineToBackendUrl('listening on http://0.0.0.0:43127'), null);
  assert.equal(lineToBackendUrl('warning: listening on http://127.0.0.1:1'), null);
});

test('accepts only an embedded frontend with the exact package build id', () => {
  const manifest = { buildId: 'build-123' };
  assert.doesNotThrow(() => validateHealth({
    status: 'ok',
    frontend_embedded: true,
    build_id: 'build-123',
    os: 'macos',
    arch: 'aarch64'
  }, { ...manifest, platform: 'darwin', arch: 'arm64' }));
  assert.throws(() => validateHealth({
    status: 'ok',
    frontend_embedded: false,
    build_id: 'build-123',
    os: 'macos',
    arch: 'aarch64'
  }, { ...manifest, platform: 'darwin', arch: 'arm64' }), /without the embedded frontend/);
  assert.throws(() => validateHealth({
    status: 'ok',
    frontend_embedded: true,
    build_id: 'old-build',
    os: 'macos',
    arch: 'aarch64'
  }, { ...manifest, platform: 'darwin', arch: 'arm64' }), /Build mismatch/);
  assert.throws(() => validateHealth({
    status: 'ok',
    frontend_embedded: true,
    build_id: 'build-123',
    os: 'macos',
    arch: 'x86_64'
  }, { ...manifest, platform: 'darwin', arch: 'arm64' }), /Platform mismatch/);

  assert.doesNotThrow(() => validateHealth({
    status: 'ok',
    frontend_embedded: true,
    build_id: 'build-123',
    os: 'linux',
    arch: 'x86_64'
  }, { ...manifest, platform: 'linux', arch: 'x64' }));
});

test('Finder-safe Python discovery prefers explicit and per-user runtimes', () => {
  const options = {
    env: { SMIDR_PYTHON: '/explicit/python' },
    manifest: { pythonPath: '/builder/python' },
    userData: '/Users/me/Library/Application Support/Smiðr',
    resourcesPath: '/Applications/Smiðr.app/Contents/Resources',
    platform: 'darwin'
  };
  assert.deepEqual(pythonCandidates(options), [
    '/explicit/python',
    '/Applications/Smiðr.app/Contents/Resources/runtime/python/bin/python3',
    '/Users/me/Library/Application Support/Smiðr/python/bin/python3',
    '/builder/python'
  ]);
  assert.equal(
    resolvePythonPath(options, (candidate) => candidate.includes('Application Support')),
    '/Users/me/Library/Application Support/Smiðr/python/bin/python3'
  );
});
