import { existsSync, readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'darwin') {
  throw new Error('macOS desktop packages must be built and verified on macOS.');
}

const mode = process.argv[2];
if (!['dir', 'dist'].includes(mode)) throw new Error('Usage: build-macos.mjs <dir|dist>');

const desktopDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const projectDir = path.resolve(desktopDir, '..');

function run(command, args, options = {}) {
  console.log(`\n> ${command} ${args.join(' ')}`);
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? desktopDir,
    env: options.env ?? process.env,
    stdio: 'inherit'
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} exited with status ${result.status}`);
}

function findApp(root) {
  if (!existsSync(root)) return null;
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const full = path.join(root, entry.name);
    if (entry.isDirectory() && entry.name === 'Smiðr.app') return full;
    if (entry.isDirectory()) {
      const nested = findApp(full);
      if (nested) return nested;
    }
  }
  return null;
}

run('npm', ['run', 'prepare:production']);
run('npm', ['run', 'smoke:backend']);

const builder = path.join(desktopDir, 'node_modules', '.bin', 'electron-builder');
const targets = mode === 'dir' ? ['dir'] : ['dmg', 'zip'];
const archFlag = process.arch === 'arm64' ? '--arm64' : '--x64';
run(builder, ['--config', 'electron-builder.config.cjs', '--mac', ...targets, archFlag]);

const appPath = findApp(path.join(desktopDir, 'dist'));
if (!appPath) throw new Error('electron-builder did not produce Smiðr.app');
const resourcesPath = path.join(appPath, 'Contents', 'Resources');
const manifest = JSON.parse(readFileSync(path.join(resourcesPath, 'runtime', 'manifest.json'), 'utf8'));
if (manifest.platform !== 'darwin' || manifest.arch !== process.arch) {
  throw new Error(`Packaged runtime architecture mismatch: ${manifest.platform}/${manifest.arch}`);
}

run(process.execPath, [path.join(desktopDir, 'scripts', 'smoke-backend.mjs')], {
  env: { ...process.env, SMIDR_PACKAGED_RESOURCES: resourcesPath }
});
run('/usr/bin/file', [path.join(resourcesPath, 'bin', 'smidr')]);

if (process.env.SMIDR_UNSIGNED_MAC_BUILD !== '1') {
  run('/usr/bin/codesign', ['--verify', '--deep', '--strict', '--verbose=2', appPath]);
}

console.log(`\nVerified macOS ${process.arch} application: ${appPath}`);
