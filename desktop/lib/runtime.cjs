const fs = require('node:fs');
const path = require('node:path');

function lineToBackendUrl(line) {
  const match = line.trim().match(/^listening on (http:\/\/127\.0\.0\.1:\d+)$/);
  return match ? match[1] : null;
}

function validateHealth(health, manifest) {
  if (!health || health.status !== 'ok') {
    throw new Error('Backend health check did not return status=ok.');
  }
  if (health.frontend_embedded !== true) {
    throw new Error('Backend was built without the embedded frontend. Refusing to open a stale build.');
  }
  if (!manifest || health.build_id !== manifest.buildId) {
    throw new Error(
      `Build mismatch: desktop expects ${manifest?.buildId ?? 'unknown'}, backend reports ${health.build_id ?? 'unknown'}.`
    );
  }
  const rustPlatforms = { darwin: 'macos', win32: 'windows' };
  const rustArchitectures = { arm64: 'aarch64', x64: 'x86_64', ia32: 'x86' };
  const expectedRustOs = rustPlatforms[manifest.platform] ?? manifest.platform;
  const expectedRustArch = rustArchitectures[manifest.arch] ?? manifest.arch;
  if (health.os !== expectedRustOs || health.arch !== expectedRustArch) {
    throw new Error(
      `Platform mismatch: package is ${manifest.platform}/${manifest.arch}, backend is ${health.os}/${health.arch}.`
    );
  }
}

function pythonCandidates({ env = process.env, manifest, userData, resourcesPath, platform = process.platform }) {
  const executable = platform === 'win32' ? 'python.exe' : 'python3';
  return [
    env.SMIDR_PYTHON,
    path.join(resourcesPath, 'runtime', 'python', 'bin', executable),
    path.join(userData, 'python', 'bin', executable),
    manifest?.pythonPath
  ].filter((candidate, index, all) => candidate && all.indexOf(candidate) === index);
}

function resolvePythonPath(options, exists = fs.existsSync) {
  return pythonCandidates(options).find((candidate) => exists(candidate)) ?? null;
}

function runtimeLayout({ packaged, resourcesPath, appDir }) {
  const root = packaged ? resourcesPath : path.join(appDir, 'generated');
  return {
    root,
    backendPath: path.join(root, 'bin', process.platform === 'win32' ? 'smidr.exe' : 'smidr'),
    runtimeDir: path.join(root, 'runtime'),
    manifestPath: path.join(root, 'runtime', 'manifest.json'),
    mcpPath: path.join(root, 'runtime', 'mcp', 'server.py'),
    pythonPackagePath: path.join(root, 'runtime', 'python-package')
  };
}

function readRuntime(layout) {
  for (const required of [
    layout.backendPath,
    layout.manifestPath,
    layout.mcpPath,
    path.join(layout.pythonPackagePath, 'pyproject.toml')
  ]) {
    if (!fs.existsSync(required)) {
      throw new Error(`Desktop runtime is incomplete: missing ${required}`);
    }
  }
  const manifest = JSON.parse(fs.readFileSync(layout.manifestPath, 'utf8'));
  if (manifest.schemaVersion !== 1 || !manifest.buildId) {
    throw new Error('Desktop runtime manifest is invalid. Rebuild the desktop package.');
  }
  return manifest;
}

module.exports = {
  lineToBackendUrl,
  validateHealth,
  pythonCandidates,
  resolvePythonPath,
  runtimeLayout,
  readRuntime
};
