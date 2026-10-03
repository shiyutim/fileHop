import { writeFile } from 'node:fs/promises';

const cdpBase = () => `http://127.0.0.1:${process.env.FILEHOP_CDP_PORT || 9237}`;

export class BrowserPage {
  pending = new Map();
  nextId = 1;
  errors = [];

  static async create() {
    const response = await fetch(`${cdpBase()}/json/new?about:blank`, { method: 'PUT' });
    const target = await response.json();
    const page = new BrowserPage();
    page.targetId = target.id;
    page.ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      page.ws.addEventListener('open', resolve, { once: true });
      page.ws.addEventListener('error', reject, { once: true });
    });
    page.ws.addEventListener('message', ({ data }) => {
      const message = JSON.parse(data);
      if (message.id) {
        const pending = page.pending.get(message.id);
        if (!pending) return;
        page.pending.delete(message.id);
        clearTimeout(pending.timer);
        if (message.error) pending.reject(new Error(JSON.stringify(message.error)));
        else pending.resolve(message.result);
      } else if (message.method === 'Runtime.exceptionThrown') {
        page.errors.push(message.params.exceptionDetails);
      }
    });
    await page.call('Page.enable');
    await page.call('Runtime.enable');
    await page.viewport(1120, 780);
    return page;
  }

  call(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP timeout: ${method}`));
      }, 10000);
      this.pending.set(id, { resolve, reject, timer });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  async evaluate(expression) {
    const result = await this.call('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  }

  async waitFor(expression, milliseconds = 8000) {
    const end = Date.now() + milliseconds;
    while (Date.now() < end) {
      if (await this.evaluate(expression)) return;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new Error(`Timed out waiting for ${expression}`);
  }

  async navigate(url = process.env.FILEHOP_URL || 'http://localhost:1432') {
    await this.call('Page.navigate', { url });
    await this.waitFor(
      "document.readyState === 'complete' && document.body.innerText.includes('FileHop')",
    );
  }

  viewport(width, height) {
    return this.call('Emulation.setDeviceMetricsOverride', {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: false,
    });
  }

  async screenshot(path) {
    const { data } = await this.call('Page.captureScreenshot', {
      format: 'png',
      captureBeyondViewport: false,
    });
    await writeFile(path, Buffer.from(data, 'base64'));
  }

  async clickText(text) {
    await this.evaluate(
      `(() => { const button = [...document.querySelectorAll('button')].find(b => b.textContent.trim().includes(${JSON.stringify(text)})); if (!button) throw new Error('Missing button ' + ${JSON.stringify(text)}); if (button.disabled) throw new Error('Disabled button ' + ${JSON.stringify(text)}); button.click(); })()`,
    );
  }

  async close() {
    this.ws.close();
    await fetch(`${cdpBase()}/json/close/${this.targetId}`);
  }
}
