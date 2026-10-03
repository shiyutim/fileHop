import { spawn } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { createRequire } from 'node:module';
import { homedir, tmpdir } from 'node:os';
import { posix, resolve, win32 } from 'node:path';
import { fileURLToPath } from 'node:url';

const projectRoot = fileURLToPath(new URL('../', import.meta.url));
const separator = '\x1f';

export function createReleaseEnvironment(env = process.env, options = {}) {
  const {
    root = projectRoot,
    home = homedir(),
    temp = tmpdir(),
    platform = process.platform,
  } = options;
  const path = platform === 'win32' ? win32 : posix;
  const childEnv = { ...env };
  // Cargo runs in src-tauri; keep relative toolchain homes stable across the
  // frontend hook, Tauri CLI and Cargo subprocesses.
  for (const name of ['CARGO_HOME', 'RUSTUP_HOME']) {
    if (childEnv[name]) childEnv[name] = path.resolve(root, 'src-tauri', childEnv[name]);
  }
  const mappings = new Map();

  function addMapping(directory, replacement) {
    if (!directory) return;
    const absolute = path.resolve(root, directory);
    const variants = new Set([absolute]);
    try {
      variants.add(realpathSync.native(absolute));
    } catch (error) {
      // Cargo/Rustup homes can be configured before they have been created.
      if (error.code !== 'ENOENT' && error.code !== 'ENOTDIR') throw error;
    }
    for (const variant of variants) {
      mappings.set(variant, replacement);
      // rustc remapping is textual; Windows source paths can use either slash.
      if (platform === 'win32') mappings.set(variant.replaceAll('\\', '/'), replacement);
    }
  }

  addMapping(home, '/home/build');
  addMapping(childEnv.CARGO_HOME || path.join(home, '.cargo'), '/cargo');
  addMapping(childEnv.RUSTUP_HOME || path.join(home, '.rustup'), '/rustup');
  for (const directory of [temp, env.TMPDIR, env.TEMP, env.TMP]) {
    addMapping(directory, '/tmp');
  }
  if (platform !== 'win32') addMapping('/tmp', '/tmp');
  addMapping(root, '/src/filehop');

  // Match Cargo's environment precedence and parsing (RUSTFLAGS is not shell
  // syntax). Custom config-file flags should be supplied through these variables.
  const existing = env.CARGO_ENCODED_RUSTFLAGS !== undefined
    ? (env.CARGO_ENCODED_RUSTFLAGS ? env.CARGO_ENCODED_RUSTFLAGS.split(separator) : [])
    : (env.RUSTFLAGS || '').split(' ').map((flag) => flag.trim()).filter(Boolean);

  // rustc uses the last matching prefix: specific roots must follow broad ones.
  const remaps = [...mappings]
    .sort(([left], [right]) => left.length - right.length)
    .map(([from, to]) => `--remap-path-prefix=${from}=${to}`);

  return {
    ...childEnv,
    CARGO_ENCODED_RUSTFLAGS: [...existing, ...remaps].join(separator),
  };
}

function main() {
  const args = process.argv.slice(2);
  if (args.includes('--debug') || args.includes('-d')) {
    throw new Error('Use pnpm tauri build --debug for a debug build; this script builds releases.');
  }
  const require = createRequire(import.meta.url);
  let cli;
  try {
    cli = require.resolve('@tauri-apps/cli/tauri.js');
  } catch {
    throw new Error('The local Tauri CLI is missing. Run pnpm install first.');
  }
  const child = spawn(process.execPath, [cli, 'build', ...args], {
    cwd: projectRoot,
    env: createReleaseEnvironment(),
    stdio: 'inherit',
  });
  child.on('error', (error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
  child.on('close', (code) => {
    process.exitCode = code ?? 1;
  });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
