import { mkdir, copyFile, readdir, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const rootDir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const scannerDir = path.join(rootDir, 'native', 'scanner');
const scannerSrcDir = path.join(scannerDir, 'src');
const scannerTarget = path.join(rootDir, 'bin', 'scanner.exe');
const builtScanner = path.join(scannerDir, 'target', 'release', 'scanner.exe');
const scannerInputs = [
  path.join(scannerDir, 'Cargo.toml'),
  path.join(scannerDir, 'Cargo.lock'),
];

async function walkFiles(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...await walkFiles(fullPath));
      continue;
    }
    files.push(fullPath);
  }
  return files;
}

async function getMtimeMs(filePath) {
  try {
    return (await stat(filePath)).mtimeMs;
  } catch {
    return 0;
  }
}

async function latestInputMtimeMs() {
  const sourceFiles = await walkFiles(scannerSrcDir);
  const candidates = [...scannerInputs, ...sourceFiles];
  const mtimes = await Promise.all(candidates.map(getMtimeMs));
  return mtimes.reduce((max, value) => Math.max(max, value), 0);
}

function runCargoBuild() {
  return new Promise((resolve, reject) => {
    const child = spawn('cargo', ['build', '--release'], {
      cwd: scannerDir,
      stdio: 'inherit',
      shell: process.platform === 'win32',
    });
    child.on('exit', (code) => {
      if (code === 0) resolve();
      else reject(new Error(`cargo build --release failed with exit code ${code ?? 'unknown'}`));
    });
    child.on('error', reject);
  });
}

async function syncScanner() {
  const latestSourceMtime = await latestInputMtimeMs();
  const targetMtime = await getMtimeMs(scannerTarget);
  const builtMtime = await getMtimeMs(builtScanner);
  const needsBuild = !targetMtime || latestSourceMtime > targetMtime || builtMtime > targetMtime;

  if (!needsBuild) {
    console.log('scanner:sync up to date');
    return;
  }

  console.log('scanner:sync rebuilding native scanner');
  await runCargoBuild();
  await mkdir(path.dirname(scannerTarget), { recursive: true });
  await copyFile(builtScanner, scannerTarget);
  console.log('scanner:sync updated bin/scanner.exe');
}

syncScanner().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
