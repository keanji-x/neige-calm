import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, writeFile, chmod, access, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { expect, it } from 'vitest';
import { previewConfig } from './config.mjs';
const run = promisify(execFile);
it.each(['owned', 'foreign-owner', 'foreign-identity'])('stops only its declared supervisor session: %s', async kind => {
  const fixture = await mkdtemp(resolve(tmpdir(), 'preview-supervisor-'));
  const config = previewConfig('5229', 'motion', 'production');
  const owner = resolve(config.fe, '..');
  const stopped = resolve(fixture, 'stopped');
  const metadata = { session: 'owned', owner: kind === 'foreign-owner' ? '/other-worktree' : owner, kind: kind === 'foreign-identity' ? 'links' : 'motion', port: config.port, mode: 'production' };
  const properties = `LoadState=loaded\nActiveState=active\nWorkingDirectory=${config.fe}\nDescription=${JSON.stringify(metadata)}\nNRestarts=0\n`;
  const supervisor = resolve(fixture, 'systemctl');
  await writeFile(supervisor, `#!${process.execPath}\nconst fs=require('node:fs');if(process.argv.includes('show'))process.stdout.write(${JSON.stringify(properties)});else if(process.argv.includes('stop'))fs.writeFileSync(${JSON.stringify(stopped)},'stopped');else process.exit(2);`);
  await chmod(supervisor, 0o755);
  try {
    const action = run(process.execPath, [resolve(import.meta.dirname, 'session.mjs'), 'stop', '--port', '5229'], { env: { ...process.env, PATH: `${fixture}:${dirname(process.execPath)}:${process.env.PATH}` } });
    if (kind === 'owned') { expect((await action).stdout).toContain('"service":"stopped"'); await expect(access(stopped)).resolves.toBeUndefined(); }
    else { await expect(action).rejects.toThrow('another session'); await expect(access(stopped)).rejects.toBeDefined(); }
  } finally { await rm(fixture, { recursive: true, force: true }); }
});

it.each([['failed', 'failed', '0'], ['active', 'dead', '123'], ['active', 'running', '0']])('refuses HTTP-ready assets for supervisor %s/%s/PID%s', async (state, substate, pid) => {
  const fixture = await mkdtemp(resolve(tmpdir(), 'preview-failed-supervisor-'));
  const metadata = resolve(fixture, 'identity.json');
  const config = previewConfig('5229', 'motion', 'production');
  const server = createServer((request, response) => {
    if (request.url === '/next/motion-preview') {
      const identity = JSON.parse(readFileSync(metadata, 'utf8')) as { session: string };
      response.setHeader('content-type', 'text/html'); response.end(`<meta name="nc-preview-session" content="${identity.session}"><div id="root"></div><script src="/next/entry.js"></script>`);
    } else { response.setHeader('content-type', 'text/javascript'); response.end('export const ready=true;'); }
  });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address(); if (address === null || typeof address === 'string') throw new Error('No port');
  const supervisor = `#!${process.execPath}\nconst fs=require('node:fs');const file=${JSON.stringify(metadata)};if(!fs.existsSync(file))console.log('LoadState=not-found\\nActiveState=inactive');else console.log('LoadState=loaded\\nActiveState=${state}\\nSubState=${substate}\\nMainPID=${pid}\\nWorkingDirectory=${config.fe}\\nDescription='+fs.readFileSync(file,'utf8')+'\\nNRestarts=3');`;
  const start = `#!${process.execPath}\nrequire('node:fs').writeFileSync(${JSON.stringify(metadata)},process.argv.find(a=>a.startsWith('--description=')).slice('--description='.length));`;
  try {
    for (const [name, content] of [['systemctl', supervisor], ['systemd-run', start]]) { const path = resolve(fixture, name); await writeFile(path, content); await chmod(path, 0o755); }
    await expect(run(process.execPath, [resolve(import.meta.dirname, 'session.mjs'), 'start', '--port', String(address.port)], { env: { ...process.env, PATH: `${fixture}:${dirname(process.execPath)}:${process.env.PATH}` } })).rejects.toThrow('failed');
  } finally { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())); await rm(fixture, { recursive: true, force: true }); }
});
