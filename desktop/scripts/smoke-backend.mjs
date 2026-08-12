import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';
import { fileURLToPath } from 'node:url';
import runtime from '../lib/runtime.cjs';

const desktopDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const packagedResources = process.env.SMIDR_PACKAGED_RESOURCES;
const layout = runtime.runtimeLayout({
  packaged: Boolean(packagedResources),
  appDir: desktopDir,
  resourcesPath: packagedResources || ''
});
const manifest = runtime.readRuntime(layout);
const env = {
  ...process.env,
  SMIDR_MCP_SERVER: layout.mcpPath,
  SMIDR_PYTHON: process.env.SMIDR_PYTHON || manifest.pythonPath
};
const child = spawn(
  layout.backendPath,
  ['--no-browser', '--port', '0', '--parent-pid', String(process.pid)],
  { cwd: layout.runtimeDir, env, stdio: ['ignore', 'pipe', 'pipe'] }
);

let stderr = '';
child.stderr.on('data', (chunk) => { stderr += chunk.toString(); });

const backendUrl = await new Promise((resolve, reject) => {
  const timer = setTimeout(() => reject(new Error(`Backend startup timed out.\n${stderr}`)), 30000);
  const lines = readline.createInterface({ input: child.stdout });
  lines.on('line', (line) => {
    const url = runtime.lineToBackendUrl(line);
    if (url) {
      clearTimeout(timer);
      resolve(url);
    }
  });
  child.once('exit', (code, signal) => reject(new Error(`Backend exited (${signal || code}).\n${stderr}`)));
});

try {
  const healthResponse = await fetch(`${backendUrl}/api/health`);
  const health = await healthResponse.json();
  runtime.validateHealth(health, manifest);

  const rootResponse = await fetch(`${backendUrl}/`);
  const html = await rootResponse.text();
  if (!rootResponse.ok || !html.includes('<title>Smiðr</title>') || html.includes('frontend not built')) {
    throw new Error('Packaged backend did not serve the production frontend.');
  }
  const asset = html.match(/src="(\/assets\/[^"]+\.js)"/i)?.[1];
  if (!asset || !(await fetch(`${backendUrl}${asset}`)).ok) {
    throw new Error('Packaged frontend JavaScript asset is missing.');
  }
  console.log(`Desktop runtime smoke test passed: ${manifest.buildId} at ${backendUrl}`);
} finally {
  child.kill('SIGTERM');
}
