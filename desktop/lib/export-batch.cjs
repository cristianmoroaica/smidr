const fs = require('node:fs');
const http = require('node:http');
const https = require('node:https');
const path = require('node:path');
const { pipeline } = require('node:stream/promises');

function request(url, options = {}) {
  return new Promise((resolve, reject) => {
    const transport = url.protocol === 'https:' ? https : http;
    const req = transport.request(url, options, resolve);
    req.once('error', reject);
    req.end();
  });
}

async function requestJson(url, options = {}) {
  const response = await request(url, options);
  const chunks = [];
  for await (const chunk of response) chunks.push(chunk);
  const body = Buffer.concat(chunks).toString('utf8');
  if (response.statusCode < 200 || response.statusCode >= 300) {
    throw new Error(`Export failed: HTTP ${response.statusCode}${body ? ` (${body})` : ''}`);
  }
  try {
    return JSON.parse(body);
  } catch (error) {
    throw new Error(`Export returned invalid JSON: ${error.message}`);
  }
}

function validateExportFiles(backendUrl, files) {
  if (!Array.isArray(files) || files.length === 0) {
    throw new Error('Export did not produce any files.');
  }

  const backend = new URL(backendUrl);
  return files.map((file) => {
    if (!file || typeof file.name !== 'string' || typeof file.url !== 'string') {
      throw new Error('Export returned an invalid file entry.');
    }
    const name = file.name;
    if (
      !name ||
      name === '.' ||
      name === '..' ||
      name.startsWith('.') ||
      path.basename(name) !== name ||
      !/\.(?:stl|step)$/i.test(name)
    ) {
      throw new Error(`Export returned an unsafe file name: ${name}`);
    }

    const source = new URL(file.url, backend);
    const segments = source.pathname.split('/');
    let decodedName;
    try {
      decodedName = decodeURIComponent(segments.at(-1));
    } catch {
      throw new Error(`Export returned an invalid file URL for ${name}.`);
    }
    if (
      source.origin !== backend.origin ||
      source.search ||
      source.hash ||
      !/^\/api\/projects\/[^/]+\/export\/[^/]+$/.test(source.pathname) ||
      decodedName !== name
    ) {
      throw new Error(`Export returned an invalid file URL for ${name}.`);
    }
    return { name, source };
  });
}

async function saveExportBatch({ backendUrl, targetDir, files }) {
  const targetStat = await fs.promises.stat(targetDir);
  if (!targetStat.isDirectory()) throw new Error('The selected export destination is not a folder.');

  const validated = validateExportFiles(backendUrl, files);
  const stagingDir = await fs.promises.mkdtemp(path.join(targetDir, '.smidr-export-'));
  try {
    for (const file of validated) {
      const response = await request(file.source);
      if (response.statusCode !== 200) {
        response.resume();
        throw new Error(`Could not save ${file.name}: HTTP ${response.statusCode}`);
      }
      await pipeline(response, fs.createWriteStream(path.join(stagingDir, file.name), { flags: 'wx' }));
    }

    for (const file of validated) {
      await fs.promises.copyFile(
        path.join(stagingDir, file.name),
        path.join(targetDir, file.name)
      );
    }
    return validated.map((file) => file.name);
  } finally {
    await fs.promises.rm(stagingDir, { recursive: true, force: true });
  }
}

module.exports = { requestJson, saveExportBatch, validateExportFiles };
