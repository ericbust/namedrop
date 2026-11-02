const assert = require('node:assert/strict');
const { test } = require('node:test');
const { execFileSync, spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const version = fs.readFileSync(path.join(root, 'Cargo.toml'), 'utf8').match(/^version = "([^"]+)"/m)[1];
const target = 'x86_64-unknown-linux-gnu';

function fixture(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'namedrop-package-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const source = path.join(dir, 'stub.c');
  const binary = path.join(dir, 'namedrop');
  fs.writeFileSync(source, `#include <stdio.h>\nint main(void) { puts("namedrop ${version}"); return 0; }\n`);
  execFileSync('cc', [source, '-o', binary]);
  return { dir, binary };
}

function packageRelease(binary, output, ...args) {
  return spawnSync('sh', [path.join(root, 'scripts/package-release.sh'), '--binary', binary, '--target', target, '--output', output, ...args], { cwd: root, encoding: 'utf8' });
}

test('default package ships a verified binary and a clean target-independent mods archive', t => {
  const { dir, binary } = fixture(t);
  const output = path.join(dir, 'dist');
  const result = packageRelease(binary, output);
  assert.equal(result.status, 0, result.stderr);
  const binaryName = `namedrop-${version}-${target}.tar.gz`;
  const modsName = `namedrop-mods-${version}.tar.gz`;
  for (const name of [binaryName, modsName]) {
    const archive = path.join(output, name);
    assert.ok(fs.statSync(archive).size > 0, name);
    execFileSync('sha256sum', ['-c', `${name}.sha256`], { cwd: output });
  }
  const members = execFileSync('tar', ['-tzf', path.join(output, modsName)], { encoding: 'utf8' }).trim().split('\n').map(member => member.replace(/^\.\//, '').replace(/\/$/, ''));
  const folders = ['namedrop', 'namedrop-name', 'namedrop-management', 'namedrop-doctor', 'namedrop-agent-status', 'namedrop-tour'];
  assert.ok(fs.existsSync(path.join(root, 'mods/namedrop/fixtures')), 'fixture exclusion needs a source sentinel');
  assert.ok(fs.existsSync(path.join(root, 'mods/namedrop/model.test.js')), 'test exclusion needs a source sentinel');
  assert.deepEqual(members.filter(member => /^mods\/[^/]+$/.test(member)).sort(), folders.map(name => `mods/${name}`).sort());
  for (const member of ['keys.json', 'RECIPE.md', ...folders.map(name => `mods/${name}/mod.json`)]) {
    assert.ok(members.includes(member), `missing ${member}`);
  }
  assert.ok(members.includes('mods/namedrop/main.js'), 'missing app payload');
  assert.ok(members.every(member => !member.endsWith('.test.js') && !member.split('/').includes('fixtures')), 'test code or fixtures leaked into mods archive');
  assert.ok(members.every(member => member === 'mods' || member.startsWith('mods/') || member === 'keys.json' || member === 'RECIPE.md'), 'unexpected archive member');
  assert.equal(fs.statSync(path.join(output, modsName)).mode & 0o777, 0o644);
  assert.equal(fs.statSync(path.join(output, binaryName)).mode & 0o777, 0o644);
});

test('binary-only omits mods and refuses an existing output without publishing another file', t => {
  const { dir, binary } = fixture(t);
  const output = path.join(dir, 'dist');
  const first = packageRelease(binary, output, '--binary-only');
  assert.equal(first.status, 0, first.stderr);
  assert.deepEqual(fs.readdirSync(output).sort(), [`namedrop-${version}-${target}.tar.gz`, `namedrop-${version}-${target}.tar.gz.sha256`]);
  const before = fs.readFileSync(path.join(output, `namedrop-${version}-${target}.tar.gz`));
  const second = packageRelease(binary, output);
  assert.equal(second.status, 73, second.stderr);
  assert.deepEqual(fs.readFileSync(path.join(output, `namedrop-${version}-${target}.tar.gz`)), before);
  assert.deepEqual(fs.readdirSync(output).sort(), [`namedrop-${version}-${target}.tar.gz`, `namedrop-${version}-${target}.tar.gz.sha256`]);
});

test('preflights every selected destination before publishing any file', t => {
  const { dir, binary } = fixture(t);
  const names = [`namedrop-${version}-${target}.tar.gz`, `namedrop-${version}-${target}.tar.gz.sha256`, `namedrop-mods-${version}.tar.gz`, `namedrop-mods-${version}.tar.gz.sha256`];
  for (const [index, name] of names.entries()) {
    const output = path.join(dir, `dist-${index}`);
    fs.mkdirSync(output);
    const sentinel = path.join(output, name);
    fs.writeFileSync(sentinel, 'keep me');
    const result = packageRelease(binary, output);
    assert.equal(result.status, 73, `${name}: ${result.stderr}`);
    assert.deepEqual(fs.readdirSync(output), [name]);
    assert.equal(fs.readFileSync(sentinel, 'utf8'), 'keep me');
  }
});
