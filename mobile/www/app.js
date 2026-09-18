import { bindServer } from './server-binding.js';

const login = document.querySelector('#login');
const resetNetwork = document.querySelector('#reset-network');
const tailnetTarget = document.querySelector('#tailnet-target');
const tailnetSettings = document.querySelector('#tailnet-settings');
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
let pendingConnection = null;
const invoke = (command, args) => {
  const call = window.__TAURI__?.core?.invoke;
  if (!call) return Promise.reject(new Error('请在 Neige 安卓 App 中连接。'));
  return call(`plugin:bundled-frontend|${command}`, args);
};
const message = (cause) => typeof cause === 'string' ? cause : cause?.message ?? '连接超时，请重新配置。';

function idle() {
  login.disabled = false;
  loginLabel.textContent = !config ? '重试读取配置' : mode.value === 'ip' ? '保存并连接 IP' : '重新连接工作区';
  mode.disabled = !config; ip.disabled = !config;
  ipSettings.hidden = mode.value !== 'ip';
  resetNetwork.hidden = mode.value !== 'tailscale';
  scan.hidden = mode.value === 'ip';
  scan.disabled = !config;
  document.body.dataset.mode = mode.value;
  tailnetSettings.hidden = mode.value !== 'tailscale' || (config?.tailnetOrigins?.length ?? 0) < 2;
  tailnetTarget.replaceChildren(...(config?.tailnetOrigins ?? []).map(origin => {
    const option = document.createElement('option'); option.value = origin; option.textContent = new URL(origin).host;
    option.selected = origin === config.tailnetOrigin; return option;
  }));
}

async function save(attempt, useDraft = mode.value === 'ip', clearSelection = false) {
  const saved = await invoke('save_connection', {
    mode: mode.value, ipOrigin: useDraft ? ip.value.trim() : config.ipOrigin, tailscaleEnabled: config?.tailscaleEnabled ?? false,
    ...(clearSelection ? { clearSelection: true } : {}),
  });
  if (attempt !== generation) return;
  config = saved;
  if (useDraft) ip.value = config.ipOrigin;
  return config;
}

async function connect(manual = false, tailnetOrigin = config?.explicitTailnet ? config.tailnetOrigin : null, confirmDirect = false) {
  const attempt = ++generation;
  const intentId = crypto.randomUUID();
  pendingConnection = intentId;
  login.disabled = true;
  scan.disabled = true;
  loginLabel.textContent = '连接中…';
  error.textContent = '';
  status.dataset.state = 'waiting';
  text.textContent = tailnetOrigin === null ? '正在连接，优先尝试 IP…' : '正在连接所选工作区…';
  try {
    const result = await invoke('attempt_connection', { intentId, ...(tailnetOrigin !== null ? { tailnetOrigin } : confirmDirect ? { confirmDirect: true } : {}) });
    if (attempt !== generation) return;
    if (!result.connected) {
      const failures = result.failures.map(item => `${item.mode === 'ip' ? 'IP' : 'Tailscale'}：${item.message}`).join('；');
      throw new Error(failures ? `连接超时或不可用，请重新配置。${failures}` : '扫描电脑上的添加手机二维码，即可加入网络并配对。');
    }
    if (tailnetOrigin !== null && (result.mode !== 'tailscale' || result.origin !== tailnetOrigin)) {
      throw new Error('连接结果不是所选工作区，请重新选择。');
    }
    status.dataset.state = 'ready';
    text.textContent = result.mode === 'ip' ? 'IP 已连接' : 'Tailscale 已连接';
    if ((result.mode === 'ip' && (manual || result.entryAvailable)) || result.resumeAvailable) {
      await bindServer(result.origin, false, intentId);
      if (attempt !== generation) return;
      location.replace(`${result.origin}/next/`);
    } else if (result.mode === 'tailscale') {
      // Reachability is not workspace authorization. Show the pairing action
      // when failover reaches a network whose workspace session is absent.
      mode.value = 'tailscale';
    }
  } catch (cause) {
    if (attempt === generation) {
      status.dataset.state = 'error';
      error.textContent = message(cause);
      text.textContent = '连接超时，请重新配置';
    }
  } finally { if (pendingConnection === intentId) pendingConnection = null; if (attempt === generation) idle(); }
}

resetNetwork.addEventListener('click', async () => {
  const attempt = ++generation; resetNetwork.disabled = true;
  try { const saved = await invoke('reset_enrollment'); if (attempt === generation) { config = saved; error.textContent = ''; idle(); } }
  catch (cause) { if (attempt === generation) error.textContent = message(cause); }
  finally { resetNetwork.disabled = false; }
});

tailnetTarget.addEventListener('change', async () => {
  const attempt = ++generation;
  const origin = tailnetTarget.value;
  try { const saved = await invoke('select_saved_tailnet', { origin }); if (attempt === generation) { config = saved; idle(); await connect(true, origin); } }
  catch (cause) { if (attempt === generation) { error.textContent = message(cause); idle(); } }
});

mode.addEventListener('change', async () => {
  const attempt = ++generation; error.textContent = ''; idle();
  try { await save(attempt, true, true); if (attempt === generation) idle(); }
  catch (cause) {
    if (attempt !== generation) return;
    if (mode.value === 'tailscale') {
      try { await save(attempt, false, true); } catch (failure) { cause = failure; }
    }
    if (attempt === generation) { error.textContent = message(cause); idle(); }
  }
});
ip.addEventListener('input', () => {
  const attempt = ++generation;
  const intentId = pendingConnection; pendingConnection = null;
  error.textContent = ''; ip.removeAttribute('aria-invalid'); idle();
  if (intentId !== null) void invoke('cancel_connection', { intentId }).catch(cause => { if (attempt === generation) error.textContent = message(cause); });
});

login.addEventListener('click', async () => {
  if (login.disabled) return;
  if (!config) { await initialize(); return; }
  const attempt = ++generation;
  login.disabled = true;
  error.textContent = '';
  try {
    await save(attempt);
    if (attempt !== generation) return;
    if (mode.value === 'ip') { await connect(true, null, true); return; }
    if (config.tailscaleEnabled) {
      if (config.legacyTailnet) {
        config = await invoke('confirm_legacy_tailnet', { origin: config.tailnetOrigin });
        if (attempt !== generation) return;
      }
      await connect(true);
    }
    else { idle(); scan.click(); }
  } catch (cause) {
    if (attempt === generation) { error.textContent = message(cause); }
  } finally { if (attempt === generation) idle(); }
});
scan.addEventListener('click', () => { ++generation; });
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
    if (config.configurationError) { error.textContent = config.configurationError; return; }
    if (config.resumeEntry) {
      const { origin, route } = config.resumeEntry;
      if (config.explicitTailnet && origin !== config.tailnetOrigin) throw new Error('保存的页面不是所选工作区，请重新连接。');
      await bindServer(origin);
      if (attempt !== generation) return;
      location.replace(`${origin}${route}`);
      return;
    }
    if (config.ipOrigin || config.tailscaleEnabled) await connect();
    else { status.dataset.state = 'waiting'; text.textContent = '扫描电脑上的添加手机二维码'; }
  } catch (cause) { if (attempt === generation) { error.textContent = message(cause); idle(); } }
}
initialize();
