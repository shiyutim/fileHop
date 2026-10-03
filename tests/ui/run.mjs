import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { access, mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { extname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { once } from 'node:events';

const root = fileURLToPath(new URL('../../', import.meta.url));
const dist = join(root, 'dist');
await access(join(dist, 'index.html'));
const candidates = [
  process.env.CHROME_PATH,
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
  '/usr/bin/chromium-browser',
  ...(process.platform === 'win32'
    ? [
        join(
          process.env.PROGRAMFILES || 'C:\\Program Files',
          'Google/Chrome/Application/chrome.exe',
        ),
      ]
    : []),
].filter(Boolean);
let chromePath;
for (const candidate of candidates) {
  try {
    await access(candidate);
    chromePath = candidate;
    break;
  } catch {
    /* Try next browser. */
  }
}
if (!chromePath) throw new Error('Install Chrome/Chromium or set CHROME_PATH to its executable.');

const mime = {
  '.html': 'text/html',
  '.js': 'application/javascript',
  '.css': 'text/css',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.json': 'application/json',
};
const server = createServer(async (request, response) => {
  try {
    const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    const path = resolve(dist, `.${pathname === '/' ? '/index.html' : pathname}`);
    if (!path.startsWith(`${dist}${sep}`)) {
      response.writeHead(403).end();
      return;
    }
    const body = await readFile(path);
    response.writeHead(200, { 'Content-Type': mime[extname(path)] || 'application/octet-stream' });
    response.end(body);
  } catch {
    response.writeHead(404).end();
  }
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
process.env.FILEHOP_URL = `http://127.0.0.1:${server.address().port}`;
const profile = await mkdtemp(join(tmpdir(), 'filehop-ui-'));
const browser = spawn(
  chromePath,
  [
    '--headless=new',
    '--disable-gpu',
    '--no-first-run',
    '--no-default-browser-check',
    '--remote-debugging-port=0',
    `--user-data-dir=${profile}`,
    'about:blank',
  ],
  { stdio: ['ignore', 'ignore', 'pipe'] },
);
let browserError;
let browserLogs = '';
browser.once('error', (error) => {
  browserError = error;
});
browser.stderr.on('data', (data) => {
  browserLogs = (browserLogs + data).slice(-4000);
});
try {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    if (browserError) throw browserError;
    if (browser.exitCode !== null) throw new Error(`Chrome exited: ${browserLogs}`);
    try {
      const activePort = await readFile(join(profile, 'DevToolsActivePort'), 'utf8');
      process.env.FILEHOP_CDP_PORT = activePort.split('\n')[0].trim();
      break;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
  if (!process.env.FILEHOP_CDP_PORT) throw new Error(`Chrome did not start: ${browserLogs}`);
  await import('./smoke.mjs');
} finally {
  if (browser.exitCode === null) {
    const exited = once(browser, 'exit');
    browser.kill('SIGTERM');
    await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 3000))]);
    if (browser.exitCode === null) browser.kill('SIGKILL');
  }
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
  await rm(profile, { recursive: true, force: true, maxRetries: 3 }).catch(() => {});
}
