import {
  chmodSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  readlinkSync,
  renameSync,
  symlinkSync,
  unlinkSync,
  writeFileSync
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'linux') throw new Error('install-linux.mjs only supports Linux.');

const desktopDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const artifact = readdirSync(path.join(desktopDir, 'dist'))
  .filter((name) => name.endsWith('.AppImage'))
  .sort()
  .at(-1);
if (!artifact) throw new Error('No AppImage found. Run npm run dist:linux first.');

const dataHome = process.env.XDG_DATA_HOME || path.join(os.homedir(), '.local', 'share');
const installDir = path.join(dataHome, 'smidr-desktop');
const applicationsDir = path.join(dataHome, 'applications');
const binDir = path.join(os.homedir(), '.local', 'bin');
const installedApp = path.join(installDir, 'Smidr.AppImage');
const installedIcon = path.join(installDir, 'icon.png');
const launcher = path.join(binDir, 'smidr-desktop');
mkdirSync(installDir, { recursive: true });
mkdirSync(applicationsDir, { recursive: true });
mkdirSync(binDir, { recursive: true });

copyFileSync(path.join(desktopDir, 'dist', artifact), `${installedApp}.new`);
chmodSync(`${installedApp}.new`, 0o755);
renameSync(`${installedApp}.new`, installedApp);
copyFileSync(path.join(desktopDir, 'build', 'icon.png'), installedIcon);

if (existsSync(launcher) || (() => { try { lstatSync(launcher); return true; } catch { return false; } })()) {
  const existing = lstatSync(launcher);
  if (!existing.isSymbolicLink() || readlinkSync(launcher) !== installedApp) {
    throw new Error(`Refusing to replace existing launcher: ${launcher}`);
  }
  unlinkSync(launcher);
}
symlinkSync(installedApp, launcher);

const desktopFile = `[Desktop Entry]\nType=Application\nName=Smiðr\nComment=AI-assisted parametric 3D modeling\nExec=${installedApp}\nIcon=${installedIcon}\nTerminal=false\nCategories=Graphics;\nKeywords=CAD;3D;CadQuery;modeling;\nStartupWMClass=smidr-desktop\n`;
const desktopPath = path.join(applicationsDir, 'smidr-desktop.desktop');
writeFileSync(`${desktopPath}.new`, desktopFile);
renameSync(`${desktopPath}.new`, desktopPath);

const legacyDesktopPath = path.join(applicationsDir, 'smidr.desktop');
if (existsSync(legacyDesktopPath)) {
  const legacyContents = readFileSync(legacyDesktopPath, 'utf8');
  if (legacyContents.includes(`Exec=${installedApp}`)) unlinkSync(legacyDesktopPath);
}

spawnSync('update-desktop-database', [applicationsDir], { stdio: 'ignore' });
console.log(`Installed Smiðr desktop app: ${installedApp}`);
console.log(`Launcher: ${launcher}`);
