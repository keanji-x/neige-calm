import { bindServer } from './server-binding.js';

const login = document.querySelector('#login');
const scan = document.querySelector('#scan');
const status = document.querySelector('#connection-status');
const text = document.querySelector('#status-text');
const error = document.querySelector('#error');
const loginDetail = document.querySelector('#login-detail');
let refreshing = false;
let loggingIn = false;
let connectionError = '';
let attemptedResume = false;
let timer;
const invoke = (command, args) => {
  const call = window.__TAURI__?.core?.invoke;
  if (!call) return Promise.reject(new Error('请在 Neige 安卓 App 中连接。'));
  return call(`plugin:bundled-frontend|${command}`, args);
};
const message = (cause) => typeof cause === 'string' ? cause : cause?.message ?? '暂时无法连接，请重试。';

async function refresh() {
  clearTimeout(timer);
  if (refreshing || loggingIn || document.hidden || scan.dataset.active || document.documentElement.classList.contains('scanning')) { timer = setTimeout(refresh, 1500); return; }
  refreshing = true;
  try {
    const connection = await invoke('connection_status');
    if (loggingIn) return;
    if (connectionError && error.textContent === connectionError) error.textContent = '';
    connectionError = '';
    const ready = connection.state === 'Running';
    status.dataset.state = ready ? 'ready' : 'waiting';
    loginDetail.textContent = ready ? '已连接 · 下次自动恢复' : '连接你的私人网络';
    if (!scan.dataset.active && !document.documentElement.classList.contains('scanning')) scan.disabled = !ready;
    text.textContent = ready ? '私人网络已连接' : connection.state === 'NeedsLogin' ? '先登录，再扫码连接工作区' : connection.state === 'NeedsMachineAuth' ? '请在 Tailscale 管理端批准此设备' : '正在恢复私人连接…';
    if (ready && connection.resumeAvailable && !attemptedResume && !document.documentElement.classList.contains('scanning')) {
      attemptedResume = true;
      scan.disabled = true;
      text.textContent = '欢迎回来，正在打开工作区…';
      await bindServer(connection.origin);
      location.replace(`${connection.origin}/next/`);
      return;
    }
  } catch (cause) {
    status.dataset.state = 'error';
    text.textContent = '连接暂时不可用';
    connectionError = message(cause);
    error.textContent = connectionError;
    scan.disabled = true;
  } finally { refreshing = false; timer = setTimeout(refresh, 1500); }
}

login.addEventListener('click', async () => {
  if (login.disabled) return;
  login.disabled = true;
  loggingIn = true;
  error.textContent = '';
  text.textContent = '正在准备登录，稍后将在浏览器继续…';
  try { await invoke('login_tailscale'); }
  catch (cause) { error.textContent = message(cause); }
  finally { login.disabled = false; loggingIn = false; refresh(); }
});
scan.addEventListener('click', () => { attemptedResume = true; });
document.addEventListener('visibilitychange', () => { if (!document.hidden) refresh(); });
refresh();
