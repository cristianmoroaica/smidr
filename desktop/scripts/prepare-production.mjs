import { createHash } from 'node:crypto';
import {
  chmodSync,
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const desktopDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const projectDir = path.resolve(desktopDir, '..');
const frontendDir = path.join(projectDir, 'frontend');
const generatedDir = path.join(desktopDir, 'generated');

function run(command, args, options = {}) {
  console.log(`\n> ${command} ${args.join(' ')}`);
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? projectDir,
    env: options.env ?? process.env,
    stdio: 'inherit'
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} exited with status ${result.status}`);
}

function output(command, args) {
  const result = spawnSync(command, args, { cwd: projectDir, encoding: 'utf8' });
  if (result.status !== 0) return 'unknown';
  return result.stdout.trim();
}

function hashFiles(entries) {
  const hash = createHash('sha256');
  const visit = (entry) => {
    const full = path.join(projectDir, entry);
    const stat = statSync(full);
    if (stat.isDirectory()) {
      for (const child of readdirSync(full).sort()) visit(path.join(entry, child));
      return;
    }
    hash.update(entry.replaceAll(path.sep, '/'));
    hash.update('\0');
    hash.update(readFileSync(full));
    hash.update('\0');
  };
  for (const entry of entries) visit(entry);
  return hash.digest('hex');
}

function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

function findPython() {
  const candidates = [
    process.env.SMIDR_PYTHON,
    path.join(projectDir, '.venv-cadquery', 'bin', 'python3'),
    path.join(projectDir, '.venv-cadquery', 'bin', 'python')
  ].filter(Boolean);
  const found = candidates.find((candidate) => existsSync(candidate));
  if (!found) {
    throw new Error('CadQuery Python is missing. Set SMIDR_PYTHON or create .venv-cadquery first.');
  }
  run(found, ['-m', 'ai3d_cad', '--version']);
  return path.resolve(found);
}

const packageJson = JSON.parse(readFileSync(path.join(desktopDir, 'package.json'), 'utf8'));
const pythonPath = findPython();

run('npm', ['ci'], { cwd: frontendDir });
run('npm', ['run', 'check'], { cwd: frontendDir });
run('npm', ['run', 'build'], { cwd: frontendDir });

const sourceHash = hashFiles([
  'Cargo.lock',
  'Cargo.toml',
  'build.rs',
  'desktop/electron-builder.config.cjs',
  'desktop/lib',
  'desktop/main.cjs',
  'desktop/preload.cjs',
  'desktop/package.json',
  'frontend/dist',
  'mcp/server.py',
  'prompts',
  'python/pyproject.toml',
  'python/src',
  'src'
]);
const gitCommit = output('git', ['rev-parse', '--short=12', 'HEAD']);
const buildId = `${packageJson.version}-${gitCommit}-${sourceHash.slice(0, 16)}`;
const buildEnv = { ...process.env, SMIDR_BUILD_ID: buildId, SMIDR_PYTHON: pythonPath };

run('cargo', ['test', '--locked', '--features', 'embed-frontend'], { env: buildEnv });
run('cargo', ['build', '--release', '--locked', '--features', 'embed-frontend'], { env: buildEnv });

const sourceBinary = path.join(projectDir, 'target', 'release', process.platform === 'win32' ? 'smidr.exe' : 'smidr');
const tempDir = mkdtempSync(path.join(desktopDir, '.generated-next-'));
try {
  const binDir = path.join(tempDir, 'bin');
  const runtimeDir = path.join(tempDir, 'runtime');
  mkdirSync(binDir, { recursive: true });
  mkdirSync(path.join(runtimeDir, 'mcp'), { recursive: true });
  mkdirSync(path.join(runtimeDir, 'python-package'), { recursive: true });

  const packagedBinary = path.join(binDir, path.basename(sourceBinary));
  copyFileSync(sourceBinary, packagedBinary);
  if (process.platform !== 'win32') chmodSync(packagedBinary, 0o755);
  cpSync(path.join(projectDir, 'mcp', 'server.py'), path.join(runtimeDir, 'mcp', 'server.py'));
  cpSync(
    path.join(projectDir, 'python', 'pyproject.toml'),
    path.join(runtimeDir, 'python-package', 'pyproject.toml')
  );
  cpSync(
    path.join(projectDir, 'python', 'src'),
    path.join(runtimeDir, 'python-package', 'src'),
    { recursive: true }
  );

  const manifest = {
    schemaVersion: 1,
    appVersion: packageJson.version,
    buildId,
    builtAt: new Date().toISOString(),
    gitCommit,
    sourceSha256: sourceHash,
    frontendSha256: sha256(path.join(frontendDir, 'dist', 'index.html')),
    backendSha256: sha256(packagedBinary),
    pythonPath,
    platform: process.platform,
    arch: os.arch()
  };
  writeFileSync(path.join(runtimeDir, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);

  rmSync(generatedDir, { recursive: true, force: true });
  renameSync(tempDir, generatedDir);
  console.log(`\nPrepared Smiðr desktop runtime ${buildId}`);
} catch (error) {
  rmSync(tempDir, { recursive: true, force: true });
  throw error;
}
