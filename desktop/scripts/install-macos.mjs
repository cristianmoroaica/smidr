import { cpSync, existsSync, mkdirSync, readdirSync, renameSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'darwin') throw new Error('install-macos.mjs only supports macOS.');

const desktopDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function findApp(root) {
  if (!existsSync(root)) return null;
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const full = path.join(root, entry.name);
    if (entry.isDirectory() && entry.name.endsWith('.app')) return full;
    if (entry.isDirectory()) {
      const nested = findApp(full);
      if (nested) return nested;
    }
  }
  return null;
}

const source = findApp(path.join(desktopDir, 'dist'));
if (!source) throw new Error('No Smiðr.app found. Run npm run package:mac first.');

const applicationsDir = path.join(os.homedir(), 'Applications');
const target = path.join(applicationsDir, 'Smiðr.app');
const staging = path.join(applicationsDir, `.Smiðr.installing-${process.pid}.app`);
const backup = path.join(applicationsDir, `.Smiðr.previous-${process.pid}.app`);
mkdirSync(applicationsDir, { recursive: true });
rmSync(staging, { recursive: true, force: true });
rmSync(backup, { recursive: true, force: true });
cpSync(source, staging, { recursive: true, preserveTimestamps: true });

try {
  if (existsSync(target)) renameSync(target, backup);
  renameSync(staging, target);
  rmSync(backup, { recursive: true, force: true });
} catch (error) {
  if (!existsSync(target) && existsSync(backup)) renameSync(backup, target);
  rmSync(staging, { recursive: true, force: true });
  throw error;
}

const opened = spawnSync('/usr/bin/open', [target], { stdio: 'inherit' });
if (opened.status !== 0) throw new Error(`open exited with status ${opened.status}`);
console.log(`Installed and opened Smiðr: ${target}`);
