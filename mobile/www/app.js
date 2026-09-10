import { bindServer } from './server-binding.js';

const login = document.querySelector('#login');
const scan = document.querySelector('#scan');
const mode = document.querySelector('#connection-mode');
const ip = document.querySelector('#ip-origin');
const ipSettings = document.querySelector('#ip-settings');
const status = document.querySelector('#connection-status');
const text = document.querySelector('#status-text');
const error = document.querySelector('#error');
const loginLabel = document.querySelector('#login-label');
let config;
let generation = 0;
let waitingForLogin = false;
const invoke = (command, args) => {
  const call = window.__TAURI__?.core?.invoke;
  if (!call) return Promise.reject(new Error('请在 Neige 安卓 App 中连接。'));
  return call(`plugin:bundled-frontend|${command}`, args);
};
const message = (cause) => typeof cause === 'string' ? cause : cause?.message ?? '连接超时，请重新配置。';

function idle() {
  login.disabled = false;
  loginLabel.textContent = !config ? '重试读取配置' : mode.value === 'ip' ? '保存并连接 IP' : '登录 Tailscale';
  mode.disabled = !config; ip.disabled = !config;
  ipSettings.hidden = mode.value !== 'ip';
  scan.hidden = mode.value === 'ip';
  scan.disabled = !config?.tailscaleEnabled;
  document.body.dataset.mode = mode.value;
}

async function save(attempt) {
  const saved = await invoke('save_connection', {
    mode: mode.value, ipOrigin: ip.value.trim(), tailscaleEnabled: config?.tailscaleEnabled ?? false,
  });
  if (attempt !== generation) return;
  config = saved;
  ip.value = config.ipOrigin;
  return config;
}

async function connect(manual = false) {
  const attempt = ++generation;
  login.disabled = true;
  scan.disabled = true;
  loginLabel.textContent = '连接中…';
  error.textContent = '';
  status.dataset.state = 'waiting';
  text.textContent = '正在连接，优先尝试 IP…';
  try {
    const result = await invoke('attempt_connection');
    if (attempt !== generation) return;
    if (!result.connected) {
      const failures = result.failures.map(item => `${item.mode === 'ip' ? 'IP' : 'Tailscale'}：${item.message}`).join('；');
      throw new Error(failures ? `连接超时或不可用，请重新配置。${failures}` : '尚未配置可用连接，请填写 IP 或登录 Tailscale。');
    }
    status.dataset.state = 'ready';
    text.textContent = result.mode === 'ip' ? 'IP 已连接' : 'Tailscale 已连接';
    if ((result.mode === 'ip' && (manual || result.entryAvailable)) || result.resumeAvailable) {
      await bindServer(result.origin);
      if (attempt !== generation) return;
      location.replace(`${result.origin}/next/`);
    }
  } catch (cause) {
    if (attempt === generation) {
      status.dataset.state = 'error';
      error.textContent = message(cause);
      text.textContent = '连接超时，请重新配置';
    }
  } finally { if (attempt === generation) idle(); }
}

mode.addEventListener('change', async () => {
  const attempt = ++generation; waitingForLogin = false; error.textContent = ''; idle();
  try { await save(attempt); if (attempt === generation) idle(); }
  catch (cause) { if (attempt === generation) error.textContent = message(cause); }
});
ip.addEventListener('input', () => { ++generation; error.textContent = ''; ip.removeAttribute('aria-invalid'); idle(); });

login.addEventListener('click', async () => {
  if (login.disabled) return;
  if (!config) { await initialize(); return; }
  const attempt = ++generation;
  login.disabled = true;
  error.textContent = '';
  try {
    await save(attempt);
    if (attempt !== generation) return;
    if (mode.value === 'ip') { await connect(true); return; }
    loginLabel.textContent = '正在登录…';
    waitingForLogin = true;
    config.tailscaleEnabled = true;
    const result = await invoke('login_tailscale');
    if (attempt !== generation) return;
    config.tailscaleEnabled = true;
    if (result.state === 'Running') { waitingForLogin = false; await connect(true); }
  } catch (cause) {
    if (attempt === generation) { waitingForLogin = false; error.textContent = message(cause); }
  } finally { if (attempt === generation) idle(); }
});
scan.addEventListener('click', () => { ++generation; waitingForLogin = false; });
document.addEventListener('visibilitychange', () => {
  if (!document.hidden && waitingForLogin) { waitingForLogin = false; connect(true); }
});

async function initialize() {
  const attempt = ++generation;
  login.disabled = true; mode.disabled = true;
  try {
    const settings = await invoke('connection_settings');
    if (attempt !== generation) return;
    config = settings;
    mode.value = config.mode;
    ip.value = config.ipOrigin;
    idle();
    await connect();
  } catch (cause) { if (attempt === generation) { error.textContent = message(cause); idle(); } }
}
initialize();
