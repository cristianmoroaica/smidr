const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const readline = require('node:readline');
const { spawn } = require('node:child_process');
const { app, BrowserWindow, dialog, ipcMain, session, shell } = require('electron');
const { requestJson, saveExportBatch, validateExportFiles } = require('./lib/export-batch.cjs');
const {
  lineToBackendUrl,
  readRuntime,
  resolvePythonPath,
  runtimeLayout,
  validateHealth
} = require('./lib/runtime.cjs');

let backend = null;
let backendUrl = null;
let mainWindow = null;
let quitting = false;
let startupFailure = null;
let lastExportDirectory = null;

app.setName('Smiðr');

if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', () => {
    if (mainWindow) {
      if (mainWindow.isMinimized()) mainWindow.restore();
      mainWindow.focus();
    }
  });
}

function getJson(url, timeoutMs = 5000) {
  return new Promise((resolve, reject) => {
    const request = http.get(url, { timeout: timeoutMs }, (response) => {
      let body = '';
      response.setEncoding('utf8');
      response.on('data', (chunk) => { body += chunk; });
      response.on('end', () => {
        if (response.statusCode !== 200) {
          reject(new Error(`Health check returned HTTP ${response.statusCode}.`));
          return;
        }
        try {
          resolve(JSON.parse(body));
        } catch (error) {
          reject(new Error(`Health check returned invalid JSON: ${error.message}`));
        }
      });
    });
    request.on('timeout', () => request.destroy(new Error('Health check timed out.')));
    request.on('error', reject);
  });
}

function stopBackend() {
  if (!backend || backend.exitCode !== null || backend.signalCode !== null) return;
  backend.kill('SIGTERM');
  const child = backend;
  const timer = setTimeout(() => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
  }, 3000);
  timer.unref();
}

function runProcess(program, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(program, args, { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    const output = [];
    const remember = (chunk) => {
      const text = chunk.toString();
      output.push(text);
      if (output.length > 80) output.shift();
      console.log(`[setup] ${text.trimEnd()}`);
    };
    child.stdout.on('data', remember);
    child.stderr.on('data', remember);
    child.once('error', reject);
    child.once('exit', (code, signal) => {
      if (code === 0) resolve();
      else reject(new Error(`${program} failed (${signal || `code ${code}`}).\n${output.join('')}`));
    });
  });
}

function findMacPython311() {
  const candidates = [
    process.env.SMIDR_BOOTSTRAP_PYTHON,
    '/opt/homebrew/bin/python3.11',
    '/usr/local/bin/python3.11',
    path.join(os.homedir(), '.local', 'bin', 'python3.11')
  ].filter(Boolean);
  return candidates.find((candidate) => fs.existsSync(candidate)) ?? null;
}

async function resolvePythonRuntime(layout, manifest) {
  const options = {
    env: process.env,
    manifest,
    userData: app.getPath('userData'),
    resourcesPath: layout.root,
    platform: process.platform
  };
  const existing = resolvePythonPath(options);
  if (existing) return existing;
  if (process.platform !== 'darwin') return null;

  const bootstrapPython = findMacPython311();
  if (!bootstrapPython) {
    throw new Error(
      'Python 3.11 is required for CadQuery. Install it with `brew install python@3.11`, then reopen Smiðr.'
    );
  }
  const choice = await dialog.showMessageBox({
    type: 'info',
    title: 'Set up Smiðr CAD runtime',
    message: 'Smiðr needs a private CadQuery Python environment.',
    detail: 'Set it up now? The first installation downloads CadQuery and may take several minutes.',
    buttons: ['Set Up', 'Quit'],
    defaultId: 0,
    cancelId: 1,
    noLink: true
  });
  if (choice.response !== 0) throw new Error('CadQuery runtime setup was cancelled.');

  const target = path.join(app.getPath('userData'), 'python');
  const staging = `${target}.installing-${process.pid}`;
  fs.rmSync(staging, { recursive: true, force: true });
  fs.mkdirSync(path.dirname(target), { recursive: true });
  try {
    await runProcess(bootstrapPython, ['-m', 'venv', '--copies', staging]);
    const stagedPython = path.join(staging, 'bin', 'python3');
    await runProcess(stagedPython, ['-m', 'pip', 'install', '--upgrade', 'pip']);
    await runProcess(stagedPython, ['-m', 'pip', 'install', layout.pythonPackagePath]);
    await runProcess(stagedPython, ['-m', 'ai3d_cad', '--version']);
    fs.rmSync(target, { recursive: true, force: true });
    fs.renameSync(staging, target);
  } catch (error) {
    fs.rmSync(staging, { recursive: true, force: true });
    throw error;
  }
  return path.join(target, 'bin', 'python3');
}

async function startBackend() {
  const layout = runtimeLayout({
    packaged: app.isPackaged,
    resourcesPath: process.resourcesPath,
    appDir: __dirname
  });
  const manifest = readRuntime(layout);
  fs.accessSync(layout.backendPath, fs.constants.X_OK);

  const env = {
    ...process.env,
    SMIDR_MCP_SERVER: layout.mcpPath
  };
  const pythonPath = await resolvePythonRuntime(layout, manifest);
  if (pythonPath) env.SMIDR_PYTHON = pythonPath;

  backend = spawn(
    layout.backendPath,
    ['--no-browser', '--port', '0', '--parent-pid', String(process.pid)],
    {
      cwd: layout.runtimeDir,
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true
    }
  );

  const stderr = [];
  const rememberError = (line) => {
    const text = line.trim();
    if (text) {
      stderr.push(text);
      if (stderr.length > 40) stderr.shift();
      console.error(`[smidr] ${text}`);
    }
  };
  readline.createInterface({ input: backend.stderr }).on('line', rememberError);

  const url = await new Promise((resolve, reject) => {
    let settled = false;
    const timeout = setTimeout(() => finish(new Error('Backend did not become ready within 30 seconds.')), 30000);
    const output = readline.createInterface({ input: backend.stdout });

    function finish(error, value) {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      output.removeAllListeners();
      if (error) reject(error); else resolve(value);
    }

    output.on('line', (line) => {
      console.log(`[smidr] ${line}`);
      const found = lineToBackendUrl(line);
      if (found) finish(null, found);
    });
    backend.once('error', (error) => finish(new Error(`Could not start backend: ${error.message}`)));
    backend.once('exit', (code, signal) => {
      finish(new Error(`Backend exited during startup (${signal || `code ${code}`}).\n${stderr.join('\n')}`));
    });
  });

  const health = await getJson(`${url}/api/health`);
  validateHealth(health, manifest);
  backendUrl = url;

  backend.on('exit', (code, signal) => {
    if (quitting || startupFailure) return;
    startupFailure = new Error(`Smiðr backend stopped unexpectedly (${signal || `code ${code}`}).`);
    dialog.showErrorBox('Smiðr stopped', startupFailure.message);
    app.quit();
  });

  return url;
}

async function createWindow() {
  if (!backendUrl) throw new Error('Backend is not ready.');

  mainWindow = new BrowserWindow({
    width: 1440,
    height: 960,
    minWidth: 1000,
    minHeight: 700,
    show: false,
    backgroundColor: '#101318',
    autoHideMenuBar: true,
    title: 'Smiðr',
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      preload: path.join(__dirname, 'preload.cjs')
    }
  });

  const allowedOrigin = new URL(backendUrl).origin;
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    if (url.startsWith('https://') || url.startsWith('http://') || url.startsWith('mailto:')) {
      void shell.openExternal(url);
    }
    return { action: 'deny' };
  });
  mainWindow.webContents.on('will-navigate', (event, url) => {
    if (new URL(url).origin !== allowedOrigin) {
      event.preventDefault();
      if (url.startsWith('https://') || url.startsWith('http://')) void shell.openExternal(url);
    }
  });
  mainWindow.once('ready-to-show', () => mainWindow?.show());
  mainWindow.on('closed', () => { mainWindow = null; });
  await mainWindow.loadURL(backendUrl);
}

ipcMain.handle('smidr:export-project', async (_event, projectId) => {
  if (!backendUrl || typeof projectId !== 'string' || !projectId.trim()) {
    throw new Error('No project is ready to export.');
  }

  const choice = await dialog.showOpenDialog(mainWindow, {
    title: 'Choose export folder',
    defaultPath: app.getPath('downloads'),
    buttonLabel: 'Export here',
    properties: ['openDirectory', 'createDirectory']
  });
  if (choice.canceled || choice.filePaths.length === 0) return { canceled: true };

  const targetDir = choice.filePaths[0];
  const exportUrl = new URL(`/api/projects/${encodeURIComponent(projectId)}/export`, backendUrl);
  const data = await requestJson(exportUrl, { method: 'POST' });
  const validatedFiles = validateExportFiles(backendUrl, data.files);
  const conflicts = validatedFiles
    .map((file) => file.name)
    .filter((name) => fs.existsSync(path.join(targetDir, name)));

  if (conflicts.length > 0) {
    const confirmation = await dialog.showMessageBox(mainWindow, {
      type: 'warning',
      title: 'Replace existing export files?',
      message: `${conflicts.length} export ${conflicts.length === 1 ? 'file already exists' : 'files already exist'} in this folder.`,
      detail: 'Replace the existing files with this export?',
      buttons: ['Replace', 'Cancel'],
      defaultId: 0,
      cancelId: 1,
      noLink: true
    });
    if (confirmation.response !== 0) return { canceled: true };
  }

  const files = await saveExportBatch({ backendUrl, targetDir, files: data.files });
  lastExportDirectory = targetDir;
  return { canceled: false, dir: targetDir, files };
});

ipcMain.handle('smidr:open-export-folder', async () => {
  if (!lastExportDirectory) throw new Error('No export folder is available yet.');
  const error = await shell.openPath(lastExportDirectory);
  if (error) throw new Error(error);
  return { path: lastExportDirectory };
});

app.whenReady().then(async () => {
  try {
    await session.defaultSession.clearCache();
    await startBackend();
    await createWindow();
  } catch (error) {
    startupFailure = error;
    stopBackend();
    const rebuildCommand = process.platform === 'darwin' ? 'npm run install:mac' : 'npm run install:linux';
    dialog.showErrorBox(
      'Smiðr could not start',
      `${error.message}\n\nRebuild with: cd desktop && ${rebuildCommand}`
    );
    app.quit();
  }
});

app.on('activate', () => {
  if (BrowserWindow.getAllWindows().length === 0 && backendUrl) void createWindow();
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});

app.on('before-quit', () => {
  quitting = true;
  stopBackend();
});
