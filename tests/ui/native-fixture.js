(() => {
  window.isTauri = true;
  const initial = {
    device: {
      id: 'local',
      name: '示例 MacBook',
      platform: 'macos',
      addresses: ['192.168.1.12'],
      port: 53318,
    },
    peers: [
      {
        id: 'peer',
        name: '书房的电脑',
        platform: 'windows',
        address: '192.168.1.25',
        port: 53318,
        lastSeen: Date.now(),
      },
    ],
    transfers: [],
    trustedDevices: [],
    saveDir: '/tmp/FileHop-received',
  };
  window.__fixture = { snapshot: initial, calls: [] };
  const pathItems = new Map();
  const remember = (files) => {
    for (const file of files) pathItems.set(file.path, file);
    return structuredClone(files);
  };
  const inspectPath = (path) => pathItems.get(path) || {
    path,
    name: path.split('/').pop(),
    size: window.__fixture.directoryPaths?.includes(path) ? 0 : 4096,
    isDirectory: window.__fixture.directoryPaths?.includes(path) || false,
  };
  window.__fixture.finishPreparation = (id) => {
    const transfer = initial.transfers.find((item) => item.id === id);
    if (!transfer || transfer.status !== 'preparing') return;
    transfer.files = transfer.files.map((file) => file.isDirectory
      ? { name: `${file.name}.zip`, size: 16384, isDirectory: false }
      : file);
    transfer.totalBytes = transfer.files.reduce((total, file) => total + file.size, 0);
    transfer.status = 'waiting';
    transfer.verificationCode = 'A42E5B81C63D29F0D61C842A37E59F10';
  };
  let nextCallback = 0;
  const callbacks = new Map();
  const listeners = new Map();
  window.__fixture.emit = (event, payload) => {
    const listener = listeners.get(event);
    if (!listener) throw new Error(`No listener for ${event}`);
    callbacks.get(listener.handler)({ event, payload, id: listener.id });
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (event) => listeners.delete(event),
  };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
    transformCallback: (callback) => {
      const id = ++nextCallback;
      callbacks.set(id, callback);
      return id;
    },
    unregisterCallback: (id) => callbacks.delete(id),
    invoke: async (command, args = {}) => {
      const state = window.__fixture;
      state.calls.push({ command, args });
      const snapshot = state.snapshot;
      if (command === 'get_snapshot') return structuredClone(snapshot);
      if (command === 'choose_files')
        return remember(state.fileSelection || [
          { name: '项目说明.pdf', size: 204800, path: '/tmp/项目说明.pdf' },
          { name: '风景照片.jpg', size: 5242880, path: '/tmp/风景照片.jpg' },
        ]);
      if (command === 'choose_folders')
        return remember(state.folderSelection || [
          { name: '项目资料', size: 0, path: '/tmp/项目资料', isDirectory: true },
        ]);
      if (command === 'inspect_files')
        return remember(args.paths.map(inspectPath));
      if (command === 'send_files' || command === 'send_text') {
        if (command === 'send_text') {
          if (state.pauseTextSend) await new Promise((resolve) => (state.finishTextSend = resolve));
          if (state.textSendError) throw new Error(state.textSendError);
        }
        const files = command === 'send_text'
          ? [{
              name: args.name
                ? /\.txt$/i.test(args.name) ? args.name : `${args.name}.txt`
                : `文案-${Date.now()}.txt`,
              size: new TextEncoder().encode(args.text).byteLength,
            }]
          : args.paths.map(inspectPath);
        const preparing = files.some((file) => file.isDirectory);
        const id = `send-${snapshot.transfers.length}`;
        snapshot.transfers.unshift({
          id,
          direction: 'send',
          peerName: args.peerName || args.address,
          files,
          totalBytes: files.reduce((total, file) => total + file.size, 0),
          transferredBytes: 0,
          status: preparing ? 'preparing' : 'waiting',
          verificationCode: preparing ? undefined : 'A42E5B81C63D29F0D61C842A37E59F10',
          localConfirmed: false,
          peerTrusted: false,
          createdAt: Date.now(),
        });
        return id;
      }
      if (command === 'respond_transfer') {
        const transfer = snapshot.transfers.find((t) => t.id === args.id);
        if (!transfer) throw new Error('任务不存在');
        if (args.accept) {
          transfer.localConfirmed = true;
          if (args.trust) {
            snapshot.trustedDevices.push({ publicKey: 'ab'.repeat(32), name: transfer.peerName, trustedAt: Date.now() });
          }
        }
        else transfer.status = 'rejected';
        return;
      }
      if (command === 'revoke_trusted_device') {
        if (state.pauseRevoke) await new Promise((resolve) => (state.finishRevoke = resolve));
        if (state.revokeError) throw new Error(state.revokeError);
        snapshot.trustedDevices = snapshot.trustedDevices.filter((device) => device.publicKey !== args.publicKey);
        return;
      }
      if (command === 'cancel_transfer') {
        const transfer = snapshot.transfers.find((t) => t.id === args.id);
        if (transfer) transfer.status = 'cancelled';
        return;
      }
      if (command === 'set_device_name') {
        snapshot.device.name = args.name;
        return;
      }
      if (command === 'choose_save_directory') {
        snapshot.saveDir = '/tmp/FileHop-selected';
        return snapshot.saveDir;
      }
      if (command === 'open_save_directory') return;
      if (command === 'clear_history') {
        snapshot.transfers = snapshot.transfers.filter((t) =>
          ['preparing', 'connecting', 'waiting', 'transferring'].includes(t.status),
        );
        return;
      }
      if (command === 'plugin:event|listen') {
        const id = ++nextCallback;
        listeners.set(args.event, { id, handler: args.handler });
        return id;
      }
      if (command.startsWith('plugin:event|')) return ++nextCallback;
      throw new Error(`Unexpected command: ${command}`);
    },
  };
})();
