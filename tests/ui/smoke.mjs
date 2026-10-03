import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { BrowserPage } from './cdp.mjs';

const artifacts = fileURLToPath(new URL('../../artifacts/ui-check', import.meta.url));
await mkdir(artifacts, { recursive: true });
const report = [];
const check = (condition, label) => {
  assert.ok(condition, label);
  report.push(label);
};
const pages = [];
const code = 'A42E5B81C63D29F0D61C842A37E59F10';
const fill = (page, selector, value) => page.evaluate(
  `(() => { const input = document.querySelector(${JSON.stringify(selector)}); input.value = ${JSON.stringify(value)}; input.dispatchEvent(new Event('input', { bubbles: true })); })()`,
);
const chooseMode = async (page, mode) => {
  await page.evaluate(
    `[...document.querySelectorAll('.send-mode button')].find(button => button.textContent.trim() === ${JSON.stringify(mode)}).click()`,
  );
  await page.waitFor(
    `document.querySelector('.send-mode button[aria-pressed=true]')?.textContent.trim() === ${JSON.stringify(mode)}`,
  );
};
try {
  const preview = await BrowserPage.create();
  pages.push(preview);
  await preview.navigate();
  await preview.waitFor(
    "document.querySelector('.dropzone') && getComputedStyle(document.querySelector('.app-shell')).display !== 'block'",
  );
  check(
    await preview.evaluate(
      "document.body.innerText.includes('桌面预览') && document.querySelector('.choose-files').disabled && document.querySelector('.choose-folders').disabled",
    ),
    'Browser preview is explicit and native file and folder controls are disabled',
  );
  check(
    await preview.evaluate(
      "!document.querySelector('.peer-button') && !document.querySelector('.transfer-card')",
    ),
    'Preview does not fabricate devices or transfers',
  );
  await preview.screenshot(`${artifacts}/preview-desktop.png`);
  await preview.viewport(820, 620);
  check(
    await preview.evaluate('document.documentElement.scrollWidth <= innerWidth'),
    'Minimum desktop width has no horizontal page overflow',
  );
  await preview.screenshot(`${artifacts}/preview-compact.png`);
  await chooseMode(preview, '文字');
  await fill(preview, '#text-content', '可以预览文字输入\nHello 👋');
  check(
    await preview.evaluate(
      "!document.querySelector('#text-content').disabled && document.querySelector('#text-content').value.includes('Hello 👋') && document.querySelector('.send-button').disabled",
    ),
    'Browser preview accepts text drafts while keeping native sending disabled',
  );
  await preview.clickText('传输记录');
  check(
    await preview.evaluate("Boolean(document.querySelector('.history-panel'))"),
    'History navigation works in preview',
  );
  await preview.clickText('偏好设置');
  check(
    await preview.evaluate("document.querySelector('input[aria-label=设备名称]').disabled"),
    'Settings preview correctly disables native changes',
  );
  check(
    await preview.evaluate("document.querySelector('.trusted-devices-empty').innerText.includes('尚未信任任何设备')"),
    'Trusted device settings show an explicit empty state',
  );
  await preview.viewport(1120, 780);
  await preview.screenshot(`${artifacts}/preview-settings.png`);

  const native = await BrowserPage.create();
  pages.push(native);
  await native.call('Page.addScriptToEvaluateOnNewDocument', {
    source: await readFile(new URL('./native-fixture.js', import.meta.url), 'utf8'),
  });
  await native.navigate();
  await native.waitFor("Boolean(document.querySelector('.peer-button'))");
  await native.clickText('选择文件');
  await native.waitFor("document.querySelectorAll('.file-queue li').length === 2");
  await native.evaluate("document.querySelector('.peer-button').click()");
  check(
    await native.evaluate("!document.querySelector('.send-button').disabled"),
    'Selecting files and a discovered peer enables send',
  );
  await native.screenshot(`${artifacts}/native-queued-fixture.png`);
  await native.evaluate("document.querySelector('.send-button').click()");
  await native.waitFor("Boolean(document.querySelector('.verification'))");
  check(
    await native.evaluate(
      `document.querySelector('.verification-code').textContent.match(/[A-Fa-f0-9]/g).join('') === '${code}'`,
    ),
    'Sender displays the complete 128-bit connection fingerprint',
  );
  check(
    await native.evaluate(
      "document.querySelector('.verification-actions .button-primary').disabled",
    ),
    'Sender cannot approve before explicitly checking the fingerprint',
  );
  check(
    await native.evaluate("!document.querySelector('.trust-checkbox input').checked"),
    'Long-term trust is an unchecked explicit opt-in on a new transfer',
  );
  await native.screenshot(`${artifacts}/native-confirm-fixture.png`);
  await native.evaluate("document.querySelector('.verification-checkbox input').click()");
  await native.clickText('确认连接');
  await native.waitFor("Boolean(document.querySelector('.confirmed-row'))");
  check(
    await native.evaluate(
      "window.__fixture.calls.some(c => c.command === 'respond_transfer' && c.args.id === 'send-0' && c.args.accept === true && c.args.trust === false) && window.__fixture.snapshot.trustedDevices.length === 0",
    ),
    'Normal sender confirmation approves only this transfer and does not establish trust',
  );
  await native.evaluate("document.querySelector('.confirmed-row button').click()");
  await native.waitFor("!document.querySelector('.verification')");
  check(
    await native.evaluate(
      "window.__fixture.calls.some(c => c.command === 'cancel_transfer' && c.args.id === 'send-0')",
    ),
    'Cancelling a confirmed waiting task invokes backend cancellation',
  );

  await native.evaluate(
    `window.__fixture.snapshot.transfers.push({id:'receive-1',direction:'receive',peerName:'书房的电脑',files:[{name:'收到的文档.pdf',size:4096}],totalBytes:4096,transferredBytes:0,status:'waiting',verificationCode:'${code}',localConfirmed:false,peerTrusted:false,createdAt:Date.now(),saveDir:'/tmp/FileHop-received'})`,
  );
  await native.waitFor("Boolean(document.querySelector('.verification-checkbox'))");
  check(
    await native.evaluate(
      "document.querySelector('.verification-actions .button-primary').disabled",
    ),
    'Receiver independently requires fingerprint confirmation',
  );
  await native.evaluate("document.querySelector('.trust-checkbox input').click()");
  check(
    await native.evaluate("document.querySelector('.verification-actions .button-primary').disabled"),
    'Opting into long-term trust still requires the first full fingerprint verification',
  );
  await native.evaluate("document.querySelector('.verification-checkbox input').click()");
  await native.clickText('确认接收');
  await native.waitFor("Boolean(document.querySelector('.confirmed-row'))");
  check(
    await native.evaluate(
      "window.__fixture.calls.some(c => c.command === 'respond_transfer' && c.args.id === 'receive-1' && c.args.accept === true && c.args.trust === true)",
    ),
    'Receiver confirmation forwards the explicit trust decision to the backend',
  );
  await native.evaluate(
    "Object.assign(window.__fixture.snapshot.transfers.find(t => t.id === 'receive-1'),{status:'transferring',transferredBytes:2048})",
  );
  await native.waitFor(
    "document.querySelector('[role=progressbar]')?.getAttribute('aria-valuenow') === '50'",
  );
  report.push('Live byte counts produce accessible 50% progress');
  await native.screenshot(`${artifacts}/native-progress-fixture.png`);
  await native.evaluate(
    "Object.assign(window.__fixture.snapshot.transfers.find(t => t.id === 'receive-1'),{status:'completed',transferredBytes:4096,completedAt:Date.now()})",
  );
  await native.waitFor("!document.querySelector('[role=progressbar]')");
  await native.clickText('传输记录');
  await native.waitFor("document.querySelectorAll('.history-list .transfer-card').length === 2");
  check(
    await native.evaluate(
      "document.querySelector('.history-list').innerText.includes('已完成') && document.querySelector('.history-list').innerText.includes('已取消')",
    ),
    'Completed and cancelled tasks appear accurately in history',
  );
  await native.clickText('已接收');
  await native.waitFor("document.querySelectorAll('.history-list .transfer-card').length === 1");
  report.push('History direction filter works');
  await native.clickText('偏好设置');
  await native.evaluate(
    "(()=>{const input=document.querySelector('input[aria-label=设备名称]');input.value='FileHop 测试电脑';input.dispatchEvent(new Event('input',{bubbles:true}));})()",
  );
  await native.clickText('保存名称');
  await native.waitFor(
    "document.querySelector('.local-device-text strong').textContent === 'FileHop 测试电脑'",
  );
  report.push('Device name settings persist through the backend API and update the sidebar');
  await native.clickText('更改');
  await native.waitFor(
    "document.querySelector('.directory-row code').textContent === '/tmp/FileHop-selected'",
  );
  report.push('Changing the receive directory updates the displayed actual backend path');
  check(
    await native.evaluate("document.querySelector('.trusted-device-list').innerText.includes('书房的电脑') && document.querySelector('.trusted-devices-section .setting-title p').innerText.includes('自动确认连接') && document.querySelector('.trusted-devices-section .setting-title p').innerText.includes('接收此设备发来的文件')"),
    'Settings list saved trusted devices and explain automatic confirmation and receipt',
  );
  await native.screenshot(`${artifacts}/native-trusted-settings-fixture.png`);
  await native.evaluate("window.__fixture.pauseRevoke = true; document.querySelector('.trusted-device-list button').click()");
  await native.waitFor("typeof window.__fixture.finishRevoke === 'function'");
  check(
    await native.evaluate("document.querySelector('.trusted-device-list button').disabled"),
    'Revoking a trusted device disables duplicate submissions while pending',
  );
  await native.evaluate("document.querySelector('.trusted-device-list button').click(); window.__fixture.pauseRevoke = false; window.__fixture.finishRevoke()");
  await native.waitFor("Boolean(document.querySelector('.trusted-devices-empty'))");
  assert.deepEqual(
    await native.evaluate("window.__fixture.calls.filter(call => call.command === 'revoke_trusted_device').map(call => call.args)"),
    [{ publicKey: 'ab'.repeat(32) }],
  );
  report.push('Revocation sends the saved device public key exactly once and refreshes the settings list');
  await native.evaluate(
    `window.__fixture.snapshot.transfers.push({id:'trusted-waiting',direction:'receive',peerName:'已信任的电脑',files:[{name:'自动接收.txt',size:4096}],totalBytes:4096,transferredBytes:0,status:'waiting',verificationCode:'${code}',localConfirmed:true,peerTrusted:true,createdAt:Date.now(),saveDir:'/tmp/FileHop-received'})`,
  );
  await native.waitFor("document.querySelector('.verification-heading')?.innerText.includes('受信任设备')");
  check(
    await native.evaluate(
      `document.querySelector('.confirmed-row').innerText.includes('本机已自动确认') && !document.querySelector('.verification-checkbox') && document.querySelector('.verification-code').textContent.match(/[A-Fa-f0-9]/g).join('') === '${code}'`,
    ),
    'Trusted waiting connections automatically confirm locally while retaining the full code for the other computer',
  );
  await native.clickText('传输文件');
  check(
    await native.evaluate("document.querySelector('.activity-empty').innerText.includes('等待对方确认') && !document.querySelector('.activity-empty').innerText.includes('请在上方确认连接')"),
    'Trusted waiting activity does not ask the local user to confirm again',
  );
  await native.screenshot(`${artifacts}/native-trusted-waiting-fixture.png`);
  await native.evaluate("document.querySelector('.confirmed-row button').click()");
  await native.waitFor("!document.querySelector('.verification')");
  await native.clickText('传输文件');
  await native.clickText('手动连接');
  await native.evaluate(
    "(()=>{const input=document.querySelector('#peer-address');input.value='192.168.1.88:53318';input.dispatchEvent(new Event('input',{bubbles:true}));})()",
  );
  await native.evaluate("document.querySelector('button[aria-label=选择此地址]').click()");
  await native.clickText('选择文件');
  await native.evaluate("document.querySelector('.send-button').click()");
  await native.waitFor(
    "window.__fixture.calls.some(c=>c.command==='send_files'&&c.args.address==='192.168.1.88:53318')",
  );
  report.push('Manual connection passes the exact selected address to the Rust backend');
  await native.viewport(820, 620);
  await native.waitFor("Boolean(document.querySelector('.verification-code'))");
  check(
    await native.evaluate('document.documentElement.scrollWidth <= innerWidth'),
    'Connection confirmation fits the minimum desktop width',
  );
  await native.screenshot(`${artifacts}/native-confirm-compact-fixture.png`);

  const folderPage = await BrowserPage.create();
  pages.push(folderPage);
  await folderPage.call('Page.addScriptToEvaluateOnNewDocument', {
    source: await readFile(new URL('./native-fixture.js', import.meta.url), 'utf8'),
  });
  await folderPage.navigate();
  await folderPage.waitFor("Boolean(document.querySelector('.peer-button'))");
  await folderPage.clickText('选择文件夹');
  await folderPage.waitFor("document.querySelectorAll('.file-queue li').length === 1");
  check(
    await folderPage.evaluate("window.__fixture.calls.some(call => call.command === 'choose_folders') && document.querySelector('.queue-file-icon.folder') && document.querySelector('.file-queue').innerText.includes('项目资料') && document.querySelector('.file-queue').innerText.includes('文件夹') && !document.querySelector('.dropzone').innerText.includes('0 B') && document.querySelector('.folder-send-hint').innerText.includes('自动压缩为 ZIP') && document.querySelector('.folder-send-hint').innerText.includes('手动解压')"),
    'Folder selection shows a folder icon, automatic ZIP behavior and unknown size instead of a zero-byte file',
  );
  await folderPage.clickText('添加文件');
  await folderPage.waitFor("document.querySelectorAll('.file-queue li').length === 3");
  await folderPage.clickText('添加文件夹');
  await folderPage.waitFor("window.__fixture.calls.filter(call => call.command === 'choose_folders').length === 2");
  check(
    await folderPage.evaluate("document.querySelectorAll('.file-queue li').length === 3 && document.querySelector('.queue-total').innerText.includes('文件夹大小待压缩')"),
    'Files and folders share one queue, duplicate folder selection is ignored and the total states the pending ZIP size',
  );
  await folderPage.evaluate("window.__fixture.directoryPaths = ['/tmp/新资料']; window.__fixture.emit('tauri://drag-drop', { paths: ['/tmp/新资料', '/tmp/项目资料', '/tmp/说明.txt'], position: { x: 200, y: 200 } })");
  await folderPage.waitFor("document.querySelectorAll('.file-queue li').length === 5");
  check(
    await folderPage.evaluate("document.querySelectorAll('.queue-file-icon.folder').length === 2"),
    'Mixed file and folder drag-and-drop appends both kinds and deduplicates existing paths',
  );
  await folderPage.viewport(820, 620);
  check(
    await folderPage.evaluate('document.documentElement.scrollWidth <= innerWidth'),
    'Folder picker actions and the mixed queue fit the minimum desktop width',
  );
  await folderPage.screenshot(`${artifacts}/native-folders-compact-fixture.png`);
  await folderPage.evaluate("document.querySelector('.peer-button').click()");
  await folderPage.waitFor("!document.querySelector('.send-button').disabled");
  await folderPage.evaluate("document.querySelector('.send-button').click()");
  await folderPage.waitFor("Boolean(document.querySelector('.preparing-progress'))");
  check(
    await folderPage.evaluate("document.querySelector('.transfer-status').innerText.includes('正在压缩') && !document.querySelector('.preparing-progress').hasAttribute('aria-valuenow') && !document.querySelector('.verification') && !document.querySelector('.file-queue') && document.querySelector('.preparing-progress').getAttribute('aria-label').includes('压缩为 ZIP') && document.querySelector('.transfer-title').innerText.includes('大小待压缩') && !document.querySelector('button[aria-label=取消传输]').disabled"),
    'Sending folders clears the queue, displays indeterminate compression with cancellation and waits before connection confirmation',
  );
  assert.deepEqual(
    await folderPage.evaluate("window.__fixture.calls.find(call => call.command === 'send_files').args.paths"),
    ['/tmp/项目资料', '/tmp/项目说明.pdf', '/tmp/风景照片.jpg', '/tmp/新资料', '/tmp/说明.txt'],
  );
  report.push('Folder sending forwards the original mixed paths to the backend for ZIP preparation');
  await folderPage.viewport(1120, 780);
  await folderPage.evaluate("document.querySelector('.preparing-details').scrollIntoView({ block: 'center' })");
  await folderPage.screenshot(`${artifacts}/native-folders-preparing-fixture.png`);
  await folderPage.evaluate("document.querySelector('button[aria-label=取消传输]').click()");
  await folderPage.waitFor("!document.querySelector('.preparing-progress')");
  check(
    await folderPage.evaluate("window.__fixture.calls.filter(call => call.command === 'cancel_transfer' && call.args.id === 'send-0').length === 1 && window.__fixture.snapshot.transfers[0].status === 'cancelled'"),
    'Cancelling during folder compression invokes backend cancellation and removes the active progress',
  );
  await folderPage.clickText('选择文件夹');
  await folderPage.waitFor("document.querySelectorAll('.file-queue li').length === 1");
  await folderPage.evaluate("document.querySelector('.send-button').click()");
  await folderPage.waitFor("Boolean(document.querySelector('.preparing-progress'))");
  await folderPage.evaluate("window.__fixture.finishPreparation('send-1')");
  await folderPage.waitFor("Boolean(document.querySelector('.verification'))");
  check(
    await folderPage.evaluate("document.querySelector('.transfer-title').innerText.includes('项目资料.zip') && document.querySelector('.transfer-title').innerText.includes('16.0 KB') && !document.querySelector('.preparing-progress') && document.querySelector('.verification-actions .button-primary').disabled"),
    'Completed compression replaces the folder with its ZIP name and actual size before the existing verification flow',
  );
  await folderPage.evaluate("document.querySelector('.verification-actions .button-quiet').click()");
  await folderPage.waitFor("!document.querySelector('.verification')");
  await folderPage.clickText('选择文件夹');
  await folderPage.waitFor("document.querySelectorAll('.file-queue li').length === 1");
  await folderPage.evaluate("window.__fixture.fileSelection = Array.from({ length: 100 }, (_, index) => ({ name: `file-${index}.txt`, path: `/tmp/file-${index}.txt`, size: 1 }))");
  await folderPage.clickText('添加文件');
  await folderPage.waitFor("document.querySelectorAll('.file-queue li').length === 100");
  check(
    await folderPage.evaluate("document.querySelectorAll('.queue-file-icon.folder').length === 1 && document.body.innerText.includes('超出的项目未添加') && [...document.querySelectorAll('.queue-actions button')].every(button => button.disabled)"),
    'The 100-item selection limit counts files and folders together and explains skipped excess items',
  );
  await folderPage.evaluate("window.__fixture.emit('tauri://drag-drop', { paths: ['/tmp/超出限制.txt'], position: { x: 200, y: 200 } })");
  await folderPage.waitFor("window.__fixture.calls.filter(call => call.command === 'inspect_files').length === 2");
  check(
    await folderPage.evaluate("document.querySelectorAll('.file-queue li').length === 100 && !document.querySelector('.file-queue').innerText.includes('超出限制.txt')"),
    'Drag-and-drop also respects the combined selection limit',
  );
  await folderPage.evaluate("document.querySelector('.file-queue li .icon-button').click()");
  await folderPage.waitFor("document.querySelectorAll('.file-queue li').length === 99");
  await folderPage.clickText('添加文件夹');
  await folderPage.waitFor("document.querySelectorAll('.file-queue li').length === 100");
  check(
    await folderPage.evaluate("document.querySelectorAll('.queue-file-icon.folder').length === 1"),
    'Removing a queued item frees capacity for a folder selection',
  );

  const textPage = await BrowserPage.create();
  pages.push(textPage);
  await textPage.call('Page.addScriptToEvaluateOnNewDocument', {
    source: await readFile(new URL('./native-fixture.js', import.meta.url), 'utf8'),
  });
  await textPage.navigate();
  await textPage.waitFor("Boolean(document.querySelector('.peer-button'))");
  await textPage.clickText('选择文件');
  await textPage.waitFor("document.querySelectorAll('.file-queue li').length === 2");
  await chooseMode(textPage, '文字');
  await textPage.waitFor("document.activeElement?.id === 'text-content'");
  const draft = '  活动文案\n第二行：你好，世界 👋🌏\n链接 https://example.com/a?b=1\n  保留缩进与结尾空格  \n';
  await fill(textPage, '#text-content', draft);
  await fill(textPage, '#text-filename', '  活动文案  ');
  check(
    await textPage.evaluate("document.querySelector('.send-button').disabled"),
    'Text sending requires a selected recipient',
  );
  await textPage.evaluate("document.querySelector('.peer-button').click()");
  await textPage.waitFor("!document.querySelector('.send-button').disabled");
  await chooseMode(textPage, '文件');
  check(
    await textPage.evaluate("document.querySelectorAll('.file-queue li').length === 2"),
    'Switching to text and back preserves the queued files',
  );
  await chooseMode(textPage, '文字');
  await textPage.clickText('传输记录');
  await textPage.clickText('偏好设置');
  await textPage.clickText('传输文件');
  check(
    await textPage.evaluate(
      `document.querySelector('#text-content')?.value === ${JSON.stringify(draft)} && document.querySelector('#text-filename').value === '  活动文案  '`,
    ),
    'Text, whitespace, filename and selected mode survive mode changes and page navigation',
  );
  await textPage.screenshot(`${artifacts}/native-text-fixture.png`);
  await textPage.viewport(820, 620);
  check(
    await textPage.evaluate('document.documentElement.scrollWidth <= innerWidth'),
    'Text composer fits the minimum desktop width without horizontal page overflow',
  );
  await textPage.screenshot(`${artifacts}/native-text-compact-fixture.png`);
  await textPage.viewport(1120, 780);

  await fill(textPage, '#text-content', ' \n\t\u3000 ');
  check(
    await textPage.evaluate("document.querySelector('.send-button').disabled"),
    'Whitespace-only text cannot be sent even with queued files and a selected recipient',
  );
  await fill(textPage, '#text-content', '🙂'.repeat(262144));
  check(
    await textPage.evaluate("!document.querySelector('.send-button').disabled"),
    'A text payload exactly at the 1 MiB UTF-8 limit can be sent',
  );
  await fill(textPage, '#text-content', `${'🙂'.repeat(262144)}a`);
  check(
    await textPage.evaluate(
      "document.querySelector('.send-button').disabled && document.querySelector('#text-content').getAttribute('aria-invalid') === 'true' && document.querySelector('#text-size').innerText.includes('超过')",
    ),
    'UTF-8 bytes above 1 MiB disable send and show an accessible size error',
  );
  await fill(textPage, '#text-content', draft);
  await textPage.evaluate("window.__fixture.snapshot.serverError = '测试：端口被占用'");
  await textPage.waitFor("document.body.innerText.includes('测试：端口被占用')");
  check(
    await textPage.evaluate("document.querySelector('.send-button').disabled"),
    'Text sending is disabled when the transfer server is unavailable',
  );
  await textPage.evaluate('delete window.__fixture.snapshot.serverError');
  await textPage.waitFor("!document.querySelector('.send-button').disabled");
  await textPage.evaluate("window.__fixture.pauseTextSend = true; document.querySelector('.send-button').click()");
  await textPage.waitFor("typeof window.__fixture.finishTextSend === 'function'");
  check(
    await textPage.evaluate(
      "document.querySelector('.send-button').disabled && document.querySelector('#text-content').disabled && document.querySelector('#text-filename').disabled && [...document.querySelectorAll('.send-mode button')].every(button => button.disabled)",
    ),
    'Pending text sends prevent duplicate submissions and draft changes',
  );
  await textPage.evaluate("document.querySelector('.send-button').click(); window.__fixture.pauseTextSend = false; window.__fixture.finishTextSend()");
  await textPage.waitFor("Boolean(document.querySelector('.verification')) && !document.querySelector('.send-button').disabled");
  const textCalls = await textPage.evaluate("window.__fixture.calls.filter(call => call.command === 'send_text')");
  assert.deepEqual(textCalls.map(call => call.args), [{
    address: '192.168.1.25:53318',
    peerName: '书房的电脑',
    text: draft,
    name: '活动文案',
  }]);
  report.push('A single send_text command preserves multiline Chinese, emoji and whitespace with a trimmed filename and selected peer');
  check(
    await textPage.evaluate(
      `document.querySelector('#text-content').value === ${JSON.stringify(draft)} && document.querySelector('#text-filename').value === '  活动文案  ' && document.querySelector('.transfer-card').innerText.includes('活动文案.txt')`,
    ),
    'Successful text sending shows TXT in connection confirmation and retains the draft for resending',
  );
  await textPage.evaluate("document.querySelector('.verification-actions .button-quiet').click()");
  await textPage.waitFor("!document.querySelector('.verification')");
  await textPage.clickText('手动连接');
  await fill(textPage, '#peer-address', '192.168.1.88:53318');
  await textPage.evaluate("document.querySelector('button[aria-label=选择此地址]').click()");
  await fill(textPage, '#text-filename', '   ');
  await textPage.evaluate("document.querySelector('.send-button').click()");
  await textPage.waitFor("window.__fixture.calls.filter(call => call.command === 'send_text').length === 2 && !document.querySelector('.send-button').disabled");
  const manualText = await textPage.evaluate("window.__fixture.calls.filter(call => call.command === 'send_text')[1].args");
  assert.deepEqual(manualText, { address: '192.168.1.88:53318', peerName: '192.168.1.88:53318', text: draft, name: null });
  report.push('Manual text sending passes the exact address and requests automatic naming for a blank filename');
  await textPage.evaluate("document.querySelector('.verification-actions .button-quiet').click()");
  await textPage.waitFor("!document.querySelector('.verification')");
  await fill(textPage, '#text-filename', '重试草稿');
  await textPage.evaluate("window.__fixture.textSendError = '测试：连接失败，请重试'; document.querySelector('.send-button').click()");
  await textPage.waitFor("document.body.innerText.includes('测试：连接失败，请重试')");
  check(
    await textPage.evaluate(
      `document.querySelector('#text-content').value === ${JSON.stringify(draft)} && document.querySelector('#text-filename').value === '重试草稿' && !document.querySelector('.send-button').disabled`,
    ),
    'A backend send error displays feedback and preserves editable text and filename for retry',
  );
  await textPage.evaluate("window.__fixture.emit('tauri://drag-drop', { paths: ['/tmp/追加文件.txt'], position: { x: 200, y: 200 } })");
  await textPage.waitFor("document.querySelectorAll('.file-queue li').length === 3");
  check(
    await textPage.evaluate("document.querySelector('.send-mode button[aria-pressed=true]').textContent.trim() === '文件'"),
    'Dropping a file while composing text switches to the preserved file queue and adds the file',
  );
  await chooseMode(textPage, '文字');
  check(
    await textPage.evaluate(`document.querySelector('#text-content').value === ${JSON.stringify(draft)}`),
    'Dropping files leaves the text draft intact',
  );
  await textPage.evaluate("document.querySelector('.clear-text').click()");
  check(
    await textPage.evaluate(
      "document.querySelector('#text-content').value === '' && document.querySelector('#text-filename').value === '' && document.querySelector('.send-button').disabled",
    ),
    'Explicitly clearing the text draft removes content and filename and disables send',
  );
  check(
    pages.every((page) => page.errors.length === 0),
    'No uncaught JavaScript exceptions across all tested states',
  );
  await writeFile(
    `${artifacts}/report.json`,
    JSON.stringify(
      {
        passed: report.length,
        checks: report,
        note: 'Native UI cases use injected command fixtures; actual transport is covered by Rust loopback tests.',
      },
      null,
      2,
    ),
  );
  console.log(JSON.stringify({ passed: report.length, checks: report }, null, 2));
} finally {
  for (const page of pages) await page.close();
}
