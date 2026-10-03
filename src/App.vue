<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from 'vue';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import type { UnlistenFn } from '@tauri-apps/api/event';
import AppIcon from './components/AppIcon.vue';
import TransferCard from './components/TransferCard.vue';
import {
  endpoint,
  formatBytes,
  isTerminal,
  platformName,
  type FileItem,
  type Snapshot,
  type Transfer,
} from './types';
import './style.css';

const native = isTauri();
const view = ref<'transfer' | 'history' | 'settings'>('transfer');
const historyFilter = ref<'all' | 'send' | 'receive'>('all');
const snapshot = ref<Snapshot>({
  device: { id: '', name: native ? '正在连接…' : '这台电脑', platform: '', addresses: [], port: 0 },
  peers: [],
  transfers: [],
  trustedDevices: [],
  saveDir: '',
});
const queue = ref<FileItem[]>([]);
const maxSelection = 100;
const sendMode = ref<'files' | 'text'>('files');
const textContent = ref('');
const textFileName = ref('');
const textInput = ref<HTMLTextAreaElement>();
const maxTextBytes = 1024 * 1024;
const selectedPeer = ref('');
const manualOpen = ref(false);
const manualAddress = ref('');
const usingManual = ref(false);
const dragging = ref(false);
const busy = ref(false);
const refreshing = ref(false);
const busyTransfers = ref<Set<string>>(new Set());
const revokingDevices = ref<Set<string>>(new Set());
const deviceName = ref('');
const savingName = ref(false);
const toast = ref<{ message: string; error: boolean } | null>(null);
const connectionError = ref('');
const speeds = ref<Record<string, number>>({});
let previousBytes = new Map<string, { bytes: number; time: number }>();
let interval: ReturnType<typeof setInterval> | undefined;
let toastTimer: ReturnType<typeof setTimeout> | undefined;
let unlistenDrop: UnlistenFn | undefined;
let currentRefresh: Promise<void> | undefined;
let disposed = false;
let loaded = false;

const currentPeer = computed(() =>
  snapshot.value.peers.find((peer) => peer.id === selectedPeer.value),
);
const targetAddress = computed(() =>
  usingManual.value
    ? manualAddress.value.trim()
    : currentPeer.value
      ? endpoint(currentPeer.value.address, currentPeer.value.port)
      : '',
);
const totalSize = computed(() => queue.value.reduce((sum, file) => sum + file.size, 0));
const folderCount = computed(() => queue.value.filter((file) => file.isDirectory).length);
const queueSizeLabel = computed(() => {
  if (!folderCount.value) return `共 ${formatBytes(totalSize.value)}`;
  return queue.value.length === folderCount.value
    ? '大小将在压缩后计算'
    : `文件 ${formatBytes(totalSize.value)} · 文件夹大小待压缩`;
});
const textSize = computed(() => new TextEncoder().encode(textContent.value).byteLength);
const textTooLarge = computed(() => textSize.value > maxTextBytes);
const hasSendContent = computed(() =>
  sendMode.value === 'text'
    ? Boolean(textContent.value.trim()) && !textTooLarge.value
    : queue.value.length > 0,
);
const active = computed(() =>
  snapshot.value.transfers
    .filter((transfer) => !isTerminal(transfer.status))
    .sort((a, b) => b.createdAt - a.createdAt),
);
const waiting = computed(() => active.value.filter((transfer) => transfer.status === 'waiting'));
const needsLocalConfirmation = computed(() => waiting.value.some((transfer) => !transfer.localConfirmed));
const running = computed(() => active.value.filter((transfer) => transfer.status !== 'waiting'));
const history = computed(() =>
  snapshot.value.transfers
    .filter((transfer) => isTerminal(transfer.status))
    .sort((a, b) => b.createdAt - a.createdAt),
);
const filteredHistory = computed(() =>
  history.value.filter(
    (transfer) => historyFilter.value === 'all' || transfer.direction === historyFilter.value,
  ),
);
const localAddresses = computed(() =>
  snapshot.value.device.addresses.map((address) => endpoint(address, snapshot.value.device.port)),
);
const canSend = computed(
  () =>
    native &&
    !busy.value &&
    !snapshot.value.serverError &&
    hasSendContent.value &&
    Boolean(targetAddress.value),
);
const networkError = computed(
  () => connectionError.value || snapshot.value.serverError || snapshot.value.discoveryError,
);

function notify(message: string, error = false) {
  toast.value = { message, error };
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(
    () => {
      toast.value = null;
    },
    error ? 7000 : 3500,
  );
}
function errorMessage(error: unknown): string {
  return error instanceof Error
    ? error.message
    : typeof error === 'string'
      ? error
      : '操作未完成，请重试。';
}
async function fetchSnapshot() {
  if (!native || disposed) return;
  try {
    const next = await invoke<Snapshot>('get_snapshot');
    if (disposed) return;
    const now = Date.now();
    const nextBytes = new Map<string, { bytes: number; time: number }>();
    const nextSpeeds: Record<string, number> = {};
    for (const transfer of next.transfers) {
      if (transfer.status !== 'transferring') continue;
      const previous = previousBytes.get(transfer.id);
      if (previous && now > previous.time) {
        const speed = Math.max(
          0,
          (transfer.transferredBytes - previous.bytes) / ((now - previous.time) / 1000),
        );
        nextSpeeds[transfer.id] = speeds.value[transfer.id]
          ? speeds.value[transfer.id] * 0.4 + speed * 0.6
          : speed;
      }
      nextBytes.set(transfer.id, { bytes: transfer.transferredBytes, time: now });
    }
    previousBytes = nextBytes;
    speeds.value = nextSpeeds;
    snapshot.value = next;
    connectionError.value = '';
    if (!loaded) {
      deviceName.value = next.device.name;
      loaded = true;
    }
  } catch (error) {
    connectionError.value = `无法连接本机服务：${errorMessage(error)}`;
  }
}
function refresh(): Promise<void> {
  if (currentRefresh) return currentRefresh;
  currentRefresh = fetchSnapshot().finally(() => {
    currentRefresh = undefined;
  });
  return currentRefresh;
}
// A snapshot started before a command may contain old state. Finish that read
// first, then fetch the command's result without overlapping polling requests.
async function refreshAfterAction() {
  if (currentRefresh) await currentRefresh;
  await refresh();
}
async function manualRefresh() {
  refreshing.value = true;
  await refresh();
  refreshing.value = false;
}
function addFiles(files: FileItem[]) {
  const paths = new Set(queue.value.map((file) => file.path));
  const unique = files.filter(
    (file) => file.path && !paths.has(file.path) && (paths.add(file.path), true),
  );
  const available = Math.max(0, maxSelection - queue.value.length);
  queue.value.push(...unique.slice(0, available));
  if (unique.length > available) {
    notify(`每次最多发送 ${maxSelection} 个文件或文件夹，超出的项目未添加。`, true);
  }
  if (files.length) {
    view.value = 'transfer';
    sendMode.value = 'files';
  }
}
async function chooseSendMode(mode: 'files' | 'text') {
  if (busy.value) return;
  sendMode.value = mode;
  if (mode === 'text') {
    await nextTick();
    textInput.value?.focus();
  }
}
function clearText() {
  textContent.value = '';
  textFileName.value = '';
  textInput.value?.focus();
}
async function chooseFiles(folders = false) {
  if (!native || busy.value) return;
  busy.value = true;
  try {
    addFiles(await invoke<FileItem[]>(folders ? 'choose_folders' : 'choose_files'));
  } catch (error) {
    notify(errorMessage(error), true);
  } finally {
    busy.value = false;
  }
}
async function inspectDrop(paths: string[]) {
  if (!native || busy.value) return;
  busy.value = true;
  try {
    addFiles(await invoke<FileItem[]>('inspect_files', { paths }));
  } catch (error) {
    notify(errorMessage(error), true);
  } finally {
    busy.value = false;
  }
}
function useManualAddress() {
  if (manualAddress.value.trim()) {
    usingManual.value = true;
    selectedPeer.value = '';
  }
}
function choosePeer(id: string) {
  selectedPeer.value = id;
  usingManual.value = false;
}
async function sendContent() {
  if (!canSend.value) return;
  const preparingFolders = sendMode.value === 'files' && folderCount.value > 0;
  busy.value = true;
  try {
    const target = {
      address: targetAddress.value,
      peerName: currentPeer.value?.name || manualAddress.value.trim(),
    };
    if (sendMode.value === 'text') {
      await invoke<string>('send_text', {
        ...target,
        text: textContent.value,
        name: textFileName.value.trim() || null,
      });
    } else {
      await invoke<string>('send_files', {
        ...target,
        paths: queue.value.map((file) => file.path),
      });
      queue.value = [];
    }
    await refreshAfterAction();
    notify(preparingFolders ? '正在压缩文件夹…' : '传输请求已发出');
  } catch (error) {
    notify(errorMessage(error), true);
  } finally {
    busy.value = false;
  }
}
async function actOnTransfer(id: string, command: string, accept?: boolean, trust = false) {
  if (busyTransfers.value.has(id)) return;
  busyTransfers.value.add(id);
  try {
    await invoke(command, accept === undefined ? { id } : { id, accept, trust: accept && trust });
    await refreshAfterAction();
  } catch (error) {
    notify(errorMessage(error), true);
  } finally {
    busyTransfers.value.delete(id);
  }
}
function respond(id: string, accept: boolean, trust = false) {
  void actOnTransfer(id, 'respond_transfer', accept, trust);
}
function cancel(id: string) {
  void actOnTransfer(id, 'cancel_transfer');
}
async function revokeTrustedDevice(publicKey: string) {
  if (!native || revokingDevices.value.has(publicKey)) return;
  revokingDevices.value.add(publicKey);
  try {
    await invoke('revoke_trusted_device', { publicKey });
    await refreshAfterAction();
    notify('已取消信任，下次传输需重新确认。');
  } catch (error) {
    notify(errorMessage(error), true);
  } finally {
    revokingDevices.value.delete(publicKey);
  }
}
async function clearHistory() {
  try {
    await invoke('clear_history');
    await refreshAfterAction();
    notify('已清空传输记录，文件仍保留在原位置。');
  } catch (error) {
    notify(errorMessage(error), true);
  }
}
async function saveDeviceName() {
  const name = deviceName.value.trim();
  if (!native || !name || savingName.value) return;
  savingName.value = true;
  try {
    await invoke('set_device_name', { name });
    await refreshAfterAction();
    deviceName.value = name;
    notify('设备名称已更新。');
  } catch (error) {
    notify(errorMessage(error), true);
  } finally {
    savingName.value = false;
  }
}
async function chooseSaveDirectory() {
  if (!native || busy.value) return;
  busy.value = true;
  try {
    const directory = await invoke<string | null>('choose_save_directory');
    if (directory) {
      await refreshAfterAction();
      notify('接收文件夹已更新。');
    }
  } catch (error) {
    notify(errorMessage(error), true);
  } finally {
    busy.value = false;
  }
}
async function openSaveDirectory() {
  if (!native) return;
  try {
    await invoke('open_save_directory');
  } catch (error) {
    notify(errorMessage(error), true);
  }
}
async function copyAddress(address: string) {
  try {
    try {
      if (!navigator.clipboard) throw new Error('Clipboard API unavailable');
      await navigator.clipboard.writeText(address);
    } catch {
      // Older desktop webviews may not expose the asynchronous clipboard API.
      const focused = document.activeElement;
      const field = document.createElement('textarea');
      field.value = address;
      field.readOnly = true;
      field.style.cssText = 'position:fixed;top:-9999px;left:-9999px;';
      document.body.append(field);
      field.select();
      try {
        if (!document.execCommand('copy')) throw new Error('Copy failed');
      } finally {
        field.remove();
        if (focused instanceof HTMLElement) focused.focus({ preventScroll: true });
      }
    }
    notify('设备地址已复制。');
  } catch {
    notify('复制失败，请在设置中手动选择并复制地址。', true);
  }
}
function fileIcon(file: FileItem) {
  if (file.isDirectory) return 'folder';
  if (/\.(png|jpe?g|gif|webp|heic|svg|avif)$/i.test(file.name)) return 'image';
  if (/\.(zip|rar|7z|tar|gz)$/i.test(file.name)) return 'archive';
  return 'file';
}
function transferProps(transfer: Transfer) {
  return { transfer, speed: speeds.value[transfer.id], busy: busyTransfers.value.has(transfer.id) };
}
onMounted(async () => {
  if (!native) return;
  await refresh();
  if (disposed) return;
  interval = setInterval(() => {
    void refresh();
  }, 750);
  try {
    const unlisten = await getCurrentWebviewWindow().onDragDropEvent((event) => {
      dragging.value = event.payload.type === 'enter' || event.payload.type === 'over';
      if (event.payload.type === 'drop') void inspectDrop(event.payload.paths);
    });
    if (disposed) unlisten();
    else unlistenDrop = unlisten;
  } catch (error) {
    notify(`拖放暂不可用，请用“选择文件”：${errorMessage(error)}`, true);
  }
});
onUnmounted(() => {
  disposed = true;
  if (interval) clearInterval(interval);
  if (toastTimer) clearTimeout(toastTimer);
  unlistenDrop?.();
});
</script>

<template>
  <div class="app-shell" @dragover.prevent @drop.prevent>
    <aside class="sidebar">
      <a class="brand" href="#" aria-label="FileHop 首页" @click.prevent="view = 'transfer'"
        ><span class="brand-symbol"><AppIcon name="transfer" :size="25" /></span
        ><span class="brand-name">FileHop<span>轻 渡</span></span></a
      >
      <nav class="navigation" aria-label="主导航">
        <button
          aria-label="传输文件"
          :class="{ selected: view === 'transfer' }"
          :aria-current="view === 'transfer' ? 'page' : undefined"
          @click="view = 'transfer'"
        >
          <AppIcon name="transfer" /><span>传输文件</span
          ><span v-if="active.length" class="nav-badge">{{ active.length }}</span>
        </button>
        <button
          aria-label="传输记录"
          :class="{ selected: view === 'history' }"
          :aria-current="view === 'history' ? 'page' : undefined"
          @click="view = 'history'"
        >
          <AppIcon name="history" /><span>传输记录</span>
        </button>
        <button
          aria-label="偏好设置"
          :class="{ selected: view === 'settings' }"
          :aria-current="view === 'settings' ? 'page' : undefined"
          @click="view = 'settings'"
        >
          <AppIcon name="settings" /><span>偏好设置</span>
        </button>
      </nav>
      <div class="local-device">
        <div class="local-device-icon">
          <AppIcon name="laptop" :size="21" /><i
            :class="{
              'is-offline': !native || Boolean(connectionError) || Boolean(snapshot.serverError),
            }"
          />
        </div>
        <div class="local-device-text">
          <strong :title="snapshot.device.name">{{ snapshot.device.name }}</strong
          ><span>{{
            native
              ? snapshot.serverError || connectionError
                ? '服务不可用'
                : snapshot.discoveryError
                  ? '本机 · 手动连接'
                  : '本机 · 可被发现'
              : '桌面预览'
          }}</span>
        </div>
        <button
          class="icon-button"
          title="设备设置"
          aria-label="打开设备设置"
          @click="view = 'settings'"
        >
          <AppIcon name="chevron" :size="15" />
        </button>
      </div>
    </aside>

    <main class="main-content">
      <header class="topbar">
        <h1>
          {{
            view === 'transfer' ? '传输文件' : view === 'history' ? '传输记录' : '偏好设置'
          }}
        </h1>
        <span
          class="connection-pill"
          :class="{ preview: !native, 'connection-warning': native && networkError }"
          ><AppIcon :name="native ? 'wifi' : 'monitor'" :size="14" />{{
            !native ? '桌面预览' : networkError ? '连接待检查' : '局域网直连'
          }}<span v-if="native && !networkError" class="status-dot"
        /></span>
      </header>
      <div v-if="networkError" class="notice notice-error" role="alert">
        <AppIcon name="alert" :size="18" />
        <div>
          <strong>{{ snapshot.serverError ? '传输服务未能启动' : '连接异常' }}</strong>
          <p>{{ networkError }}</p>
          <p v-if="!snapshot.serverError && !connectionError">可使用设备地址手动连接。</p>
        </div>
        <button class="text-button" @click="manualRefresh">重试</button>
      </div>
      <section v-if="waiting.length" class="incoming-section" aria-label="需要确认的连接">
        <div class="section-heading">
          <h2><span class="attention-dot" />{{ waiting.length }} 个连接等待确认</h2>
        </div>
        <TransferCard
          v-for="transfer in waiting"
          :key="transfer.id"
          v-bind="transferProps(transfer)"
          @respond="respond"
          @cancel="cancel"
        />
      </section>

      <template v-if="view === 'transfer'">
        <div class="workspace-grid">
          <section class="send-panel" aria-labelledby="send-heading">
            <div class="section-heading">
              <h2 id="send-heading">{{ sendMode === 'text' ? '发送文字' : '发送文件' }}</h2>
            </div>
            <div class="send-mode" role="group" aria-label="发送内容类型">
              <button
                :aria-pressed="sendMode === 'files'"
                :disabled="busy"
                @click="chooseSendMode('files')"
              >
                <AppIcon name="folder" :size="15" />文件
              </button>
              <button
                :aria-pressed="sendMode === 'text'"
                :disabled="busy"
                @click="chooseSendMode('text')"
              >
                <AppIcon name="file" :size="15" />文字
              </button>
            </div>
            <div
              v-if="sendMode === 'files'"
              class="dropzone"
              :class="{ dragging, 'has-files': queue.length }"
            >
              <template v-if="!queue.length"
                ><div class="file-illustration" aria-hidden="true">
                  <span class="illustration-orbit orbit-one" /><span
                    class="illustration-orbit orbit-two"
                  />
                  <div class="illustration-file file-back"><AppIcon name="image" :size="33" /></div>
                  <div class="illustration-file file-front"><AppIcon name="file" :size="37" /></div>
                  <div class="illustration-plus"><AppIcon name="plus" :size="15" /></div>
                </div>
                <h3>{{ dragging ? '松开以添加' : '拖入文件或文件夹' }}</h3>
                <div class="selection-actions">
                  <button
                    class="button button-primary choose-files"
                    :disabled="!native || busy"
                    @click="chooseFiles()"
                  >
                    <AppIcon name="plus" :size="17" />选择文件
                  </button>
                  <button
                    class="button button-quiet choose-folders"
                    :disabled="!native || busy"
                    @click="chooseFiles(true)"
                  >
                    <AppIcon name="folder" :size="17" />选择文件夹
                  </button>
                </div>
                <span v-if="busy" class="dropzone-hint">正在读取…</span></template
              >
              <template v-else
                ><div class="queue-heading">
                  <span>已选 {{ queue.length }} 项</span>
                  <div class="queue-actions">
                    <button class="text-button" :disabled="busy || queue.length >= maxSelection" @click="chooseFiles()">
                      <AppIcon name="plus" :size="14" />添加文件
                    </button>
                    <button class="text-button choose-folders" :disabled="busy || queue.length >= maxSelection" @click="chooseFiles(true)">
                      <AppIcon name="folder" :size="14" />添加文件夹
                    </button>
                  </div>
                </div>
                <ul class="file-queue">
                  <li v-for="(file, index) in queue" :key="file.path || index">
                    <span class="queue-file-icon" :class="fileIcon(file)"
                      ><AppIcon :name="fileIcon(file)" :size="23"
                    /></span>
                    <div>
                      <strong :title="file.name">{{ file.name }}</strong
                      ><span>{{ file.isDirectory ? '文件夹' : formatBytes(file.size) }}</span>
                    </div>
                    <button
                      class="icon-button"
                      :disabled="busy"
                      :aria-label="`移除 ${file.name}`"
                      @click="queue.splice(index, 1)"
                    >
                      <AppIcon name="close" :size="16" />
                    </button>
                  </li>
                </ul>
                <p v-if="folderCount" class="folder-send-hint">文件夹自动压缩为 ZIP，对方收到后手动解压。</p>
                <div class="queue-total">
                  <span>{{ queueSizeLabel }}</span
                  ><button class="text-button muted" :disabled="busy" @click="queue = []">
                    清空
                  </button>
                </div></template
              >
            </div>
            <div v-else class="text-composer">
              <div class="text-composer-heading">
                <label for="text-content">要发送的文字</label>
                <button
                  class="text-button muted clear-text"
                  :disabled="busy || (!textContent && !textFileName)"
                  @click="clearText"
                >
                  清空
                </button>
              </div>
              <textarea
                id="text-content"
                ref="textInput"
                v-model="textContent"
                placeholder="输入或粘贴文字"
                :disabled="busy"
                :aria-invalid="textTooLarge"
                aria-describedby="text-size text-format"
                spellcheck="false"
              />
              <div
                id="text-size"
                class="text-size"
                :class="{ 'is-error': textTooLarge }"
                aria-live="polite"
              >
                <span v-if="textTooLarge">文字超过 1 MB，请缩短后发送。</span>
                <span>{{ formatBytes(textSize) }} / 1 MB</span>
              </div>
              <label class="text-filename-label" for="text-filename">文件名<span>可选</span></label>
              <input
                id="text-filename"
                v-model="textFileName"
                placeholder="留空自动命名"
                :disabled="busy"
                aria-describedby="text-format"
                autocomplete="off"
                spellcheck="false"
              />
              <p id="text-format" class="text-format">以 .txt 文件发送</p>
            </div>
            <div class="send-bottom">
              <div>
                <p>
                  {{
                    targetAddress
                      ? `发送给 ${usingManual ? targetAddress : currentPeer?.name}`
                      : '选择接收设备'
                  }}
                </p>
              </div>
              <button
                class="button button-primary send-button"
                :disabled="!canSend"
                @click="sendContent"
              >
                {{
                  busy && hasSendContent
                    ? '正在连接'
                    : sendMode === 'text'
                      ? '发送文字'
                      : '发送文件'
                }}<AppIcon name="arrow" :size="17" />
              </button>
            </div>
          </section>
          <section class="devices-panel" aria-labelledby="devices-heading">
            <div class="section-heading">
              <h2 id="devices-heading">
                附近的设备<span v-if="snapshot.peers.length" class="count-badge">{{
                  snapshot.peers.length
                }}</span>
              </h2>
              <button
                class="icon-button"
                :disabled="!native || refreshing"
                title="刷新设备"
                aria-label="刷新设备"
                @click="manualRefresh"
              >
                <AppIcon name="refresh" :size="16" :class="{ spinning: refreshing }" />
              </button>
            </div>
            <div v-if="!snapshot.peers.length" class="devices-empty">
              <div class="device-radar" aria-hidden="true">
                <span /><span /><AppIcon name="laptop" :size="30" /><i />
              </div>
              <strong>{{
                !native ? '暂无设备' : snapshot.discoveryError ? '自动发现暂不可用' : '正在查找设备…'
              }}</strong>
              <p>在同一局域网内打开 FileHop</p>
            </div>
            <ul v-else class="peer-list">
              <li v-for="peer in snapshot.peers" :key="peer.id">
                <button
                  class="peer-button"
                  :class="{ 'peer-selected': selectedPeer === peer.id && !usingManual }"
                  :aria-pressed="selectedPeer === peer.id && !usingManual"
                  @click="choosePeer(peer.id)"
                >
                  <span class="peer-icon"
                    ><AppIcon
                      :name="peer.platform.toLowerCase().includes('win') ? 'monitor' : 'laptop'"
                      :size="25" /></span
                  ><span class="peer-info"
                    ><strong>{{ peer.name }}</strong
                    ><span
                      >{{ platformName(peer.platform) }}<span class="middot">·</span
                      >{{ peer.address }}</span
                    ></span
                  ><span class="peer-check"
                    ><AppIcon
                      v-if="selectedPeer === peer.id && !usingManual"
                      name="check"
                      :size="12"
                  /></span>
                </button>
              </li>
            </ul>
            <div class="manual-connect">
              <button
                class="manual-toggle"
                :aria-expanded="manualOpen"
                aria-controls="manual-form"
                @click="manualOpen = !manualOpen"
              >
                <AppIcon name="link" :size="17" /><span>{{
                  usingManual ? '已选择手动地址' : '手动连接'
                }}</span
                ><AppIcon name="chevron" :size="14" :class="{ expanded: manualOpen }" />
              </button>
              <form
                v-if="manualOpen"
                id="manual-form"
                class="manual-form"
                @submit.prevent="useManualAddress"
              >
                <label for="peer-address">另一台电脑的设备地址</label>
                <div>
                  <input
                    id="peer-address"
                    v-model="manualAddress"
                    placeholder="192.168.1.8:53318"
                    spellcheck="false"
                    autocomplete="off"
                    :disabled="!native"
                    @input="usingManual = false"
                  /><button
                    class="icon-button"
                    type="submit"
                    :disabled="!native || !manualAddress.trim()"
                    aria-label="选择此地址"
                  >
                    <AppIcon name="arrow" :size="17" />
                  </button>
                </div>
                <p v-if="!usingManual">在对方的「偏好设置」中复制地址。</p>
              </form>
            </div>
          </section>
        </div>
        <section class="activity-section" aria-labelledby="activity-heading">
          <div class="section-heading">
            <h2 id="activity-heading">
              正在传输<span v-if="running.length" class="count-badge">{{ running.length }}</span>
            </h2>
            <button class="text-button muted" @click="view = 'history'">
              查看记录<AppIcon name="chevron" :size="14" />
            </button>
          </div>
          <div v-if="!running.length" class="activity-empty">
            <span><AppIcon name="transfer" :size="23" /></span>
            <div>
              <strong>{{
                needsLocalConfirmation ? '请在上方确认连接' : waiting.length ? '等待对方确认' : '暂无传输'
              }}</strong>
            </div>
          </div>
          <TransferCard
            v-for="transfer in running"
            :key="transfer.id"
            v-bind="transferProps(transfer)"
            @respond="respond"
            @cancel="cancel"
          />
        </section>
      </template>

      <template v-else-if="view === 'history'">
        <section class="history-panel">
          <div class="history-toolbar">
            <div class="segmented-control" aria-label="筛选传输记录">
              <button
                v-for="filter in ['all', 'send', 'receive'] as const"
                :key="filter"
                :class="{ active: historyFilter === filter }"
                :aria-pressed="historyFilter === filter"
                @click="historyFilter = filter"
              >
                {{ filter === 'all' ? '全部' : filter === 'send' ? '已发送' : '已接收' }}
              </button>
            </div>
            <button
              class="text-button muted"
              :disabled="!native || !history.length"
              @click="clearHistory"
            >
              <AppIcon name="trash" :size="15" />清空记录
            </button>
          </div>
          <div v-if="!filteredHistory.length" class="large-empty">
            <span class="large-empty-icon"><AppIcon name="history" :size="35" /></span>
            <h2>暂无传输记录</h2>
            <button class="button button-secondary" @click="view = 'transfer'">
              去传文件<AppIcon name="arrow" :size="16" />
            </button>
          </div>
          <div v-else class="history-list">
            <TransferCard
              v-for="transfer in filteredHistory"
              :key="transfer.id"
              v-bind="transferProps(transfer)"
              @respond="respond"
              @cancel="cancel"
            />
          </div>
        </section>
        <p class="footnote">
          <AppIcon name="info" :size="14" />仅显示本次启动后的记录；清空记录不会删除文件。
        </p>
      </template>

      <template v-else>
        <section class="settings-panel" aria-label="设备与传输设置">
          <form class="setting-section" @submit.prevent="saveDeviceName">
            <div class="setting-title">
              <span><AppIcon name="laptop" :size="21" /></span>
              <div>
                <h2>设备名称</h2>
              </div>
            </div>
            <div class="setting-input-row">
              <input
                v-model="deviceName"
                aria-label="设备名称"
                placeholder="设备名称"
                maxlength="40"
                :disabled="!native"
              /><button
                class="button button-secondary"
                type="submit"
                :disabled="
                  !native ||
                  savingName ||
                  !deviceName.trim() ||
                  deviceName.trim() === snapshot.device.name
                "
              >
                {{ savingName ? '保存中…' : '保存名称' }}
              </button>
            </div>
          </form>
          <div class="setting-section">
            <div class="setting-title">
              <span><AppIcon name="folder" :size="21" /></span>
              <div>
                <h2>接收文件夹</h2>
              </div>
            </div>
            <div class="directory-row">
              <code :title="snapshot.saveDir">{{
                snapshot.saveDir || '暂无保存位置'
              }}</code
              ><button
                class="button button-secondary button-small"
                :disabled="!native || busy"
                @click="chooseSaveDirectory"
              >
                更改</button
              ><button
                class="icon-button"
                :disabled="!native"
                aria-label="打开接收文件夹"
                title="打开接收文件夹"
                @click="openSaveDirectory"
              >
                <AppIcon name="folderOpen" :size="19" />
              </button>
            </div>
          </div>
          <div class="setting-section trusted-devices-section">
            <div class="setting-title">
              <span><AppIcon name="shield" :size="21" /></span>
              <div>
                <h2>受信任的设备</h2>
                <p>自动确认连接，并接收此设备发来的文件。</p>
              </div>
            </div>
            <ul v-if="snapshot.trustedDevices.length" class="trusted-device-list" aria-label="受信任的设备">
              <li v-for="device in snapshot.trustedDevices" :key="device.publicKey">
                <div>
                  <strong>{{ device.name }}</strong>
                  <code :title="device.publicKey">{{ device.publicKey.slice(0, 8) }}…{{ device.publicKey.slice(-8) }}</code>
                </div>
                <button
                  class="button button-secondary button-small"
                  :disabled="!native || revokingDevices.has(device.publicKey)"
                  :aria-label="`取消信任 ${device.name}`"
                  @click="revokeTrustedDevice(device.publicKey)"
                >
                  {{ revokingDevices.has(device.publicKey) ? '处理中…' : '取消信任' }}
                </button>
              </li>
            </ul>
            <p v-else class="trusted-devices-empty">尚未信任任何设备</p>
          </div>
          <div class="setting-section">
            <div class="setting-title">
              <span><AppIcon name="wifi" :size="21" /></span>
              <div>
                <h2>本机设备地址</h2>
                <p>用于另一台电脑的「手动连接」。</p>
              </div>
            </div>
            <div v-if="localAddresses.length" class="address-list">
              <div v-for="address in localAddresses" :key="address">
                <code>{{ address }}</code
                ><button
                  class="text-button"
                  :aria-label="`复制地址 ${address}`"
                  @click="copyAddress(address)"
                >
                  <AppIcon name="copy" :size="15" />复制
                </button>
              </div>
            </div>
            <div v-else class="address-empty">
              {{
                native
                  ? '暂无局域网地址，请检查网络。'
                  : '请在桌面应用中查看'
              }}
            </div>
          </div>
        </section>
        <div class="about-strip">
          <span class="brand-symbol small"><AppIcon name="transfer" :size="18" /></span>
          <div>
            <strong>FileHop <span>轻渡</span></strong>
          </div>
          <span>版本 0.1.0</span>
        </div>
      </template>
      <footer class="content-footer">
        <span v-if="!native"
          ><AppIcon name="info" :size="13" />预览模式，传输请使用桌面应用。</span
        ><button
          v-else
          class="text-button muted"
          :disabled="!localAddresses.length"
          @click="localAddresses[0] && copyAddress(localAddresses[0])"
        >
          <AppIcon name="copy" :size="12" />{{ localAddresses[0] || '等待网络地址' }}</button
        >
      </footer>
    </main>
    <div v-if="dragging" class="drag-overlay">
      <AppIcon name="plus" :size="36" /><strong>松开，添加文件</strong>
    </div>
    <Transition name="toast"
      ><div
        v-if="toast"
        class="toast-message"
        :class="{ 'toast-error': toast.error }"
        :role="toast.error ? 'alert' : 'status'"
      >
        <AppIcon :name="toast.error ? 'alert' : 'check'" :size="18" /><span>{{
          toast.message
        }}</span
        ><button class="icon-button" aria-label="关闭提示" @click="toast = null">
          <AppIcon name="close" :size="15" />
        </button></div
    ></Transition>
  </div>
</template>
