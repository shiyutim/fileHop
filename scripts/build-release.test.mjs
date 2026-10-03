import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createReleaseEnvironment } from './build-release.mjs';

const separator = '\x1f';
const flags = (environment) => environment.CARGO_ENCODED_RUSTFLAGS.split(separator);

test('preserves encoded arguments, including spaces, and Cargo environment precedence', () => {
  const original = {
    CARGO_ENCODED_RUSTFLAGS: '--cfg=label="with spaces"\x1f-Copt-level=2',
    RUSTFLAGS: '--cfg=ignored',
  };
  const result = flags(createReleaseEnvironment(original));
  assert.deepEqual(result.slice(0, 2), ['--cfg=label="with spaces"', '-Copt-level=2']);
  assert.ok(!result.includes('--cfg=ignored'));
  assert.equal(original.CARGO_ENCODED_RUSTFLAGS, '--cfg=label="with spaces"\x1f-Copt-level=2');
  assert.ok(!flags(createReleaseEnvironment({ ...original, CARGO_ENCODED_RUSTFLAGS: '' }))
    .includes('--cfg=ignored'));
  assert.deepEqual(flags(createReleaseEnvironment({ RUSTFLAGS: '  --cfg=keep   -Copt-level=2  ' }))
    .slice(0, 2), ['--cfg=keep', '-Copt-level=2']);
});

test('resolves relative toolchain homes from the Cargo working directory', () => {
  const original = { CARGO_HOME: '../cargo cache', RUSTUP_HOME: '../../rustup cache' };
  const environment = createReleaseEnvironment(original, {
    platform: 'win32',
    root: 'C:\\build\\FileHop',
    home: 'C:\\Users\\builder',
    temp: 'C:\\Temp',
  });
  assert.equal(environment.CARGO_HOME, 'C:\\build\\FileHop\\cargo cache');
  assert.equal(environment.RUSTUP_HOME, 'C:\\build\\rustup cache');
  assert.ok(flags(environment).includes('--remap-path-prefix=C:\\build\\FileHop\\cargo cache=/cargo'));
  assert.ok(flags(environment).includes('--remap-path-prefix=C:\\build\\rustup cache=/rustup'));
  assert.equal(original.CARGO_HOME, '../cargo cache');
});

test('covers both Windows separators without splitting paths containing spaces', () => {
  const result = flags(createReleaseEnvironment({}, {
    platform: 'win32',
    root: 'C:\\Users\\Build User\\FileHop',
    home: 'C:\\Users\\Build User',
    temp: 'C:\\Users\\Build User\\AppData\\Local\\Temp',
  }));
  for (const prefix of ['C:\\Users\\Build User\\FileHop', 'C:/Users/Build User/FileHop']) {
    assert.ok(result.includes(`--remap-path-prefix=${prefix}=/src/filehop`));
  }
  assert.ok(result.indexOf('--remap-path-prefix=C:\\Users\\Build User=/home/build')
    < result.indexOf('--remap-path-prefix=C:\\Users\\Build User\\FileHop=/src/filehop'));
});

test('rustc emits remapped paths for project aliases, dependencies, home and temporary files', (t) => {
  const fixture = mkdtempSync(join(tmpdir(), 'filehop remap '));
  t.after(() => rmSync(fixture, { recursive: true, force: true }));
  const home = join(fixture, 'private home');
  const root = join(home, 'project with spaces');
  const alias = join(fixture, 'checkout alias');
  const cargo = join(home, '.cargo');
  const rustup = join(home, '.rustup');
  const temp = join(home, 'temporary files');
  for (const directory of [root, cargo, rustup, temp]) mkdirSync(directory, { recursive: true });
  symlinkSync(root, alias, process.platform === 'win32' ? 'junction' : 'dir');

  const modules = { cargo, rustup, temp, home };
  for (const directory of Object.values(modules)) {
    writeFileSync(join(directory, 'fixture.rs'), 'pub const FILE: &str = file!();\n');
  }
  const source = [
    '#[cfg(not(label = "with spaces"))] compile_error!("existing rustflags were lost");',
    ...Object.entries(modules).map(([name, directory]) =>
      `#[path = ${JSON.stringify(join(directory, 'fixture.rs'))}] mod ${name};`),
    'pub fn paths() -> [&\'static str; 5] { [file!(), cargo::FILE, rustup::FILE, temp::FILE, home::FILE] }',
  ].join('\n');
  const input = join(root, 'main.rs');
  const output = join(fixture, 'fixture.o');
  writeFileSync(input, source);
  const environment = createReleaseEnvironment({
    CARGO_HOME: cargo,
    RUSTUP_HOME: rustup,
    CARGO_ENCODED_RUSTFLAGS: '--cfg=label="with spaces"',
    RUSTFLAGS: '--cfg=ignored',
  }, { root: alias, home, temp });
  execFileSync('rustc', [
    '--crate-name=remap_fixture', '--crate-type=lib', '--emit=obj', '-Cdebuginfo=2',
    realpathSync.native(input), '-o', output, ...flags(environment),
  ], { stdio: 'pipe' });
  const object = readFileSync(output);
  for (const directory of [root, alias, home, cargo, rustup, temp]) {
    assert.ok(!object.includes(directory), `object leaked ${directory}`);
    assert.ok(!object.includes(directory.replaceAll('\\', '/')), `object leaked normalized ${directory}`);
  }
  for (const replacement of ['/src/filehop', '/cargo', '/rustup', '/tmp', '/home/build']) {
    assert.ok(object.includes(replacement), `missing ${replacement} in object`);
  }
});
