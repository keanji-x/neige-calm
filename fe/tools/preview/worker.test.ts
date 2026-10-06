import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { stat, rm } from 'node:fs/promises';
import { resolve } from 'node:path';
import { expect, it } from 'vitest';

it('keeps a foreground worker artifact untouched when another raw launch occupies its port', async () => {
  const reservation = createServer(); await new Promise<void>(resolve => reservation.listen(0, '127.0.0.1', resolve));
  const address = reservation.address(); if (address === null || typeof address === 'string') throw new Error('No port');
  await new Promise<void>(resolve => reservation.close(() => resolve()));
  const children: ReturnType<typeof spawn>[] = []; const directories: string[] = [];
  const launch = () => { const child = spawn(process.execPath, [resolve(import.meta.dirname, 'server.mjs'), String(address.port), 'motion', 'production'], { stdio: ['ignore', 'pipe', 'pipe'] }); children.push(child); let output = ''; child.stdout?.on('data', data => { output += String(data); for (const line of output.split('\n').slice(0, -1).filter(line => line.startsWith('{"worker":"built"'))) { const value = JSON.parse(line) as { artifacts: string }; if (!directories.includes(value.artifacts)) directories.push(value.artifacts); } }); return child; };
  try {
    const first = launch();
    const ready = await new Promise<{ url: string; artifacts: string }>((resolve, reject) => {
      let output = ''; first.once('exit', code => reject(new Error(`Worker exited ${code}: ${output}`)));
      first.stdout?.on('data', data => { output += String(data); const line = output.split('\n').slice(0, -1).find(line => line.startsWith('{"worker":"ready"')); if (line !== undefined) resolve(JSON.parse(line) as { url: string; artifacts: string }); });
    });

    const artifact = resolve(ready.artifacts, 'index.html'); const before = (await stat(artifact)).mtimeMs;
    const original = await (await fetch(ready.url)).text();
    const second = launch(); const exit = await new Promise<number | null>(resolve => second.once('exit', resolve));
    expect(exit).toBe(1);
    expect((await stat(artifact)).mtimeMs).toBe(before);
    expect(await (await fetch(ready.url)).text()).toBe(original);
  } finally {
    for (const child of children) { if (child.exitCode === null && child.signalCode === null) { const exited = new Promise<void>(resolve => child.once('exit', () => resolve())); child.kill('SIGTERM'); await exited; } }
    for (const directory of directories) await rm(directory, { recursive: true, force: true });
  }
}, 60000);
