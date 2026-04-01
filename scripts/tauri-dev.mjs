import { createServer } from 'node:net';
import { spawn } from 'node:child_process';
import { rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const HOST = '127.0.0.1';
const DEFAULT_PORT = 5173;
const MAX_PORT = 5272;

function tauriBinary() {
  return process.platform === 'win32'
    ? path.join(process.cwd(), 'node_modules', '.bin', 'tauri.cmd')
    : path.join(process.cwd(), 'node_modules', '.bin', 'tauri');
}

function canListen(port) {
  return new Promise((resolve) => {
    const server = createServer();
    server.once('error', () => resolve(false));
    server.once('listening', () => {
      server.close(() => resolve(true));
    });
    server.listen(port, HOST);
  });
}

async function resolvePort() {
  const requested = process.env.VITE_PORT ? Number(process.env.VITE_PORT) : null;
  if (requested) {
    const available = await canListen(requested);
    if (!available) {
      throw new Error(`VITE_PORT=${requested} is not available on ${HOST}.`);
    }
    return requested;
  }

  for (let port = DEFAULT_PORT; port <= MAX_PORT; port += 1) {
    if (await canListen(port)) {
      return port;
    }
  }

  throw new Error(`No available dev port found in ${DEFAULT_PORT}-${MAX_PORT}.`);
}

async function main() {
  const port = await resolvePort();
  const tempConfigPath = path.join(
    os.tmpdir(),
    `wipeout-tauri-dev-${process.pid}-${port}.json`
  );

  writeFileSync(
    tempConfigPath,
    JSON.stringify(
      {
        build: {
          devUrl: `http://${HOST}:${port}`,
        },
      },
      null,
      2
    )
  );

  const child = spawn(
    tauriBinary(),
    ['dev', '--config', tempConfigPath, ...process.argv.slice(2)],
    {
      cwd: process.cwd(),
      env: {
        ...process.env,
        VITE_PORT: String(port),
        VITE_STRICT_PORT: 'true',
      },
      shell: process.platform === 'win32',
      stdio: 'inherit',
    }
  );

  const cleanup = () => {
    try {
      rmSync(tempConfigPath, { force: true });
    } catch {
      // Ignore cleanup errors on process teardown.
    }
  };

  child.on('exit', (code, signal) => {
    cleanup();
    if (signal) {
      process.kill(process.pid, signal);
      return;
    }
    process.exit(code ?? 0);
  });

  child.on('error', (error) => {
    cleanup();
    console.error(error);
    process.exit(1);
  });

  for (const eventName of ['SIGINT', 'SIGTERM']) {
    process.on(eventName, () => {
      cleanup();
      if (!child.killed) {
        child.kill(eventName);
      }
    });
  }

  console.log(`Using Tauri dev server port ${port}.`);
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
});
