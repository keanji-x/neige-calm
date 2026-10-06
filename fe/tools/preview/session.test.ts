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
