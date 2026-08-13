const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const { saveExportBatch, validateExportFiles } = require('../lib/export-batch.cjs');

test('validates same-origin STL and STEP export URLs', () => {
  assert.deepEqual(
    validateExportFiles('http://127.0.0.1:43127', [
      { name: 'drag_arm.stl', url: '/api/projects/demo/export/drag_arm.stl' },
      { name: 'drag_arm.step', url: '/api/projects/demo/export/drag_arm.step' }
    ]).map((file) => file.name),
    ['drag_arm.stl', 'drag_arm.step']
  );

  assert.throws(
    () => validateExportFiles('http://127.0.0.1:43127', [
      { name: '../secret.step', url: '/api/projects/demo/export/secret.step' }
    ]),
    /unsafe file name/
  );
  assert.throws(
    () => validateExportFiles('http://127.0.0.1:43127', [
      { name: 'part.step', url: 'https://example.com/api/projects/demo/export/part.step' }
    ]),
    /invalid file URL/
  );
});

test('saves a complete export batch into one destination folder', async (t) => {
  const tempDir = await fs.promises.mkdtemp(path.join(os.tmpdir(), 'smidr-export-test-'));
  t.after(() => fs.promises.rm(tempDir, { recursive: true, force: true }));

  const payloads = new Map([
    ['/api/projects/demo/export/assembly.stl', 'solid assembly'],
    ['/api/projects/demo/export/drag_arm.step', 'STEP data']
  ]);
  const server = http.createServer((req, res) => {
    const body = payloads.get(req.url);
    if (body === undefined) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { 'content-type': 'application/octet-stream' });
    res.end(body);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));

  const { port } = server.address();
  const files = [
    { name: 'assembly.stl', url: '/api/projects/demo/export/assembly.stl' },
    { name: 'drag_arm.step', url: '/api/projects/demo/export/drag_arm.step' }
  ];
  const saved = await saveExportBatch({
    backendUrl: `http://127.0.0.1:${port}`,
    targetDir: tempDir,
    files
  });

  assert.deepEqual(saved, ['assembly.stl', 'drag_arm.step']);
  assert.equal(await fs.promises.readFile(path.join(tempDir, 'assembly.stl'), 'utf8'), 'solid assembly');
  assert.equal(await fs.promises.readFile(path.join(tempDir, 'drag_arm.step'), 'utf8'), 'STEP data');
  assert.deepEqual(
    (await fs.promises.readdir(tempDir)).sort(),
    ['assembly.stl', 'drag_arm.step']
  );
});
