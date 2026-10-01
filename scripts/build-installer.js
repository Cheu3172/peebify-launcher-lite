#!/usr/bin/env node

// ------------ Installer Build Script ------------
// Builds the whole Lite setup exe in one go: the launcher, the helper exes and DLL, then the installer stub, and
// attaches the payload to the stub as dist/installer/Peebify-Launcher-Lite-Setup-<version>.exe, then verifies it.

'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const REPO_ROOT = path.resolve(__dirname, '..');
const readJson = (file) => JSON.parse(fs.readFileSync(file, 'utf-8'));
const PKG = readJson(path.join(REPO_ROOT, 'package.json'));
const TAURI_CONF = readJson(path.join(REPO_ROOT, 'src-tauri', 'tauri.conf.json'));
const VERSION = PKG.version;
const BUILD_TYPE = PKG.buildType || 'stable';
const MAIN_BINARY = `${TAURI_CONF.productName}.exe`;
const TARGET_RELEASE = path.join(REPO_ROOT, 'target', 'release');
const LOG = '[build-installer]';

const HELPER_ARTIFACTS = [
    'peebify-fps-helper.exe',
    'peebify_helpers.dll',
    'peebify-mod-loader.exe',
    'peebify-overlay-helper.exe',
];

function fail(msg) {
    console.error(`${LOG} ${msg}`);
    process.exit(1);
}

// A static CRT means the launcher starts on a PC without the Visual C++ redistributable, and the path
// remaps keep this machine's user and folder names out of the binaries. The checkout path may hold a
// space, so the flags travel through CARGO_ENCODED_RUSTFLAGS, which cargo prefers over RUSTFLAGS.
function rustEnv(extra = {}) {
    const cargoHome = process.env.CARGO_HOME || path.join(os.homedir(), '.cargo');
    const remaps = [
        [os.homedir(), 'home'],
        [cargoHome, 'cargo'],
        [REPO_ROOT, 'peebify'],
    ].flatMap(([from, to]) => ['--remap-path-prefix', `${from}=${to}`]);
    const flags = ['-C', 'target-feature=+crt-static', ...remaps];
    const env = { ...process.env, ...extra, CARGO_ENCODED_RUSTFLAGS: flags.join('\x1f') };
    delete env.RUSTFLAGS;
    return env;
}

function run(cmd, args, opts = {}) {
    console.log(`${LOG} $ ${cmd} ${args.join(' ')}`);
    const res = spawnSync(cmd, args, { cwd: REPO_ROOT, stdio: 'inherit', ...opts });
    if (res.error) fail(`${cmd} could not be started: ${res.error.message}`);
    if (res.status !== 0) fail(`${cmd} exited with ${res.status}${res.signal ? ` (signal ${res.signal})` : ''}`);
}

function assertDepsInstalled(label, dir, binaries) {
    const missing = binaries.filter((b) => !fs.existsSync(path.join(dir, 'node_modules', '.bin', b)));
    if (!missing.length) return;
    const where = path.relative(REPO_ROOT, dir).replace(/\\/g, '/') || '.';
    fail(`${label} dependencies are not installed (missing: ${missing.join(', ')}).\n         Run:  npm --prefix ${where} install`);
}

assertDepsInstalled('root', REPO_ROOT, ['tauri']);
assertDepsInstalled('webui', path.join(REPO_ROOT, 'webui'), ['tsc', 'vite']);

console.log(`${LOG} building ${VERSION}`);
const tauriCli = path.join(REPO_ROOT, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');
run(process.execPath, [tauriCli, 'build', '--no-bundle'], {
    env: rustEnv({ PEEBIFY_BUILD_TYPE: BUILD_TYPE, PEEBIFY_VERSION: VERSION }),
});

const launcherExe = path.join(TARGET_RELEASE, 'peebify-launcher.exe');
if (!fs.existsSync(launcherExe)) fail(`launcher exe not found at ${launcherExe}`);

const payloadDir = path.join(REPO_ROOT, 'dist', 'payload');
fs.rmSync(payloadDir, { recursive: true, force: true });
fs.mkdirSync(path.join(payloadDir, 'resources'), { recursive: true });
fs.copyFileSync(launcherExe, path.join(payloadDir, MAIN_BINARY));

for (const [src, dest] of Object.entries((TAURI_CONF.bundle && TAURI_CONF.bundle.resources) || {})) {
    const from = path.resolve(REPO_ROOT, 'src-tauri', src);
    const to = path.join(payloadDir, dest);
    if (!fs.existsSync(from)) fail(`bundle resource missing: ${from}`);
    fs.mkdirSync(path.dirname(to), { recursive: true });
    fs.cpSync(from, to, { recursive: true });
}

const helperEnv = rustEnv({ PEEBIFY_VERSION: VERSION });
run('cargo', ['build', '-p', 'peebify-helpers', '--bins', '--release', '--features', 'overlay-runtime'], { env: helperEnv });
run('cargo', ['rustc', '-p', 'peebify-helpers', '--lib', '--release', '--features', 'stub', '--crate-type', 'cdylib'], { env: helperEnv });
for (const file of HELPER_ARTIFACTS) {
    const from = path.join(TARGET_RELEASE, file);
    if (!fs.existsSync(from)) fail(`helper artifact missing: ${from}`);
    fs.copyFileSync(from, path.join(payloadDir, 'resources', file));
}

run('cargo', ['build', '-p', 'peebify-installer', '--release'], { env: rustEnv({ PEEBIFY_VERSION: VERSION }) });
const stub = path.join(TARGET_RELEASE, 'peebify-installer.exe');
if (!fs.existsSync(stub)) fail(`installer stub not found at ${stub}`);

const outDir = path.join(REPO_ROOT, 'dist', 'installer');
fs.mkdirSync(outDir, { recursive: true });
const outExe = path.join(outDir, `Peebify-Launcher-Lite-Setup-${VERSION}.exe`);
run(stub, [
    'pack',
    '--stub', stub,
    '--payload-dir', payloadDir,
    '--out', outExe,
    '--version', VERSION,
    '--main-binary', MAIN_BINARY,
]);
run(stub, ['inspect', '--file', outExe]);

console.log('');
console.log(`${LOG} built ${outExe}`);
console.log(`${LOG} size: ${(fs.statSync(outExe).size / 1024 / 1024).toFixed(2)} MB`);
