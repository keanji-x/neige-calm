import { randomUUID } from 'node:crypto';
import { execFile } from 'node:child_process';
import { promisify, parseArgs } from 'node:util';
import { resolve } from 'node:path';
import { previewConfig, previewReadiness } from './config.mjs';
const run = promisify(execFile);
const { values, positionals } = parseArgs({ allowPositionals: true, options: { port: { type: 'string' }, kind: { type: 'string', default: 'motion' }, mode: { type: 'string', default: 'production' } } });
const [action] = positionals;
if (positionals.length !== 1 || !['start', 'status', 'stop', 'restart'].includes(action)) throw new Error('Expected start/status/stop/restart --port <port> [--kind motion|links] [--mode production|development]');
const config = previewConfig(values.port, values.kind, values.mode); const owner = resolve(config.fe, '..');
/** @param {string[]} args */
const systemctl = (...args) => run('systemctl', ['--user', ...args]);
const inspect = async () => {
  const { stdout } = await systemctl('show', config.unit, '--property=LoadState,ActiveState,SubState,WorkingDirectory,Description,NRestarts,MainPID');
  const properties = Object.fromEntries(stdout.trim().split('\n').map(line => { const index = line.indexOf('='); return [line.slice(0, index), line.slice(index + 1)]; }));
  if (properties.LoadState !== 'not-found') {
    let identity; try { identity = JSON.parse(properties.Description); } catch { throw new Error('Unit is not an owned preview session'); }
    if (properties.WorkingDirectory !== config.fe || identity.owner !== owner || identity.kind !== config.kind || identity.port !== config.port) throw new Error(`Preview belongs to another session: ${properties.WorkingDirectory}`);
    if (action === 'start' && identity.mode !== config.mode) throw new Error('Preview mode differs; use restart explicitly');
  }
  return properties;
};
/** @param {Record<string, string>} properties */
const running = properties => properties.ActiveState === 'active' && properties.SubState === 'running' && Number(properties.MainPID) > 0;
const current = await inspect();
if (action === 'stop' || action === 'restart') {
  if (current.LoadState !== 'not-found') await systemctl('stop', config.unit);
  if (action === 'stop') { console.log(JSON.stringify({ service: 'stopped', forwarding: 'not-verified', unit: config.unit, owner })); process.exit(0); }
}
if (action === 'start' || action === 'restart') {
  if (action === 'restart' || current.LoadState === 'not-found') {
    const revision = (await run('git', ['rev-parse', 'HEAD'], { cwd: owner })).stdout.trim(); const dirty = (await run('git', ['status', '--porcelain'], { cwd: owner })).stdout.trim() !== '';
    const identity = JSON.stringify({ session: randomUUID(), owner, revision, dirty, kind: config.kind, mode: config.mode, port: config.port });
    await run('systemd-run', ['--user', '--collect', `--unit=${config.unit}`, `--description=${identity}`, `--property=WorkingDirectory=${config.fe}`, '--property=Restart=always', '--property=RestartSec=3s', '--property=StartLimitIntervalSec=60s', '--property=StartLimitBurst=3', process.execPath, resolve(import.meta.dirname, 'server.mjs'), String(config.port), config.kind, config.mode, JSON.parse(identity).session]);
  } else if (current.ActiveState !== 'active') throw new Error('Preview is offline; use restart explicitly');
  const deadline = Date.now() + 60000; let last;
  while (Date.now() < deadline) {
    const properties = await inspect();
    if (properties.ActiveState === 'failed' || properties.LoadState === 'not-found' || (properties.ActiveState === 'active' && !running(properties))) throw new Error('Preview supervisor failed before readiness');
    if (running(properties)) {
      try {
        const ready = await previewReadiness(config, JSON.parse(properties.Description).session);
        const verified = await inspect();
        if (running(verified) && verified.MainPID === properties.MainPID) {
          console.log(JSON.stringify({ ...ready, unit: config.unit, identity: JSON.parse(verified.Description), restarts: Number(verified.NRestarts) })); process.exit(0);
        }
      } catch (error) { last = error; }
    }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  throw new Error(`Preview failed readiness: ${last}`);
}
const properties = await inspect(); const identity = properties.LoadState === 'not-found' ? null : JSON.parse(properties.Description); let health;
if (running(properties)) { try { health = await previewReadiness(config, identity.session); const verified = await inspect(); if (!running(verified) || verified.MainPID !== properties.MainPID) throw new Error('Preview changed during readiness'); } catch (error) { health = { service: 'unhealthy', reason: String(error), forwarding: 'not-verified' }; } }
else health = { service: 'offline', forwarding: 'not-verified' };
console.log(JSON.stringify({ ...health, unit: config.unit, identity, state: properties.ActiveState, restarts: Number(properties.NRestarts) }));
