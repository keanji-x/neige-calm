import { pairingDestination } from './pairing-url.js';

const scan = document.querySelector('#scan');
const cancelScan = document.querySelector('#cancel-scan');
const confirm = document.querySelector('#pair-confirm');
const error = document.querySelector('#scan-error');
let candidate = null;
let generation = 0;
let settleCancel = null;

function restoreLauncher() {
  scan.disabled = false;
  cancelScan.hidden = true;
  document.documentElement.classList.remove('scanning');
}

scan.addEventListener('click', async () => {
  const invoke = window.__TAURI__?.core?.invoke;
  error.textContent = '';
  candidate = null;
  confirm.hidden = true;
  if (!invoke) { error.textContent = '请在 Neige 安卓 App 中使用扫码。'; return; }
  const attempt = ++generation;
  scan.disabled = true;
  try {
    const permission = await invoke('plugin:barcode-scanner|request_permissions');
    if (attempt !== generation) return;
    if (permission.camera !== 'granted') throw new Error('请允许相机权限后重试。');
    cancelScan.hidden = false;
    document.documentElement.classList.add('scanning');
    const cancelled = new Promise((resolve) => { settleCancel = () => resolve(null); });
    // These are the official plugin's guest arguments; no remote origin is
    // granted the native capability used by this packaged launcher.
    const result = await Promise.race([
      invoke('plugin:barcode-scanner|scan', { formats: ['QR_CODE'], cameraDirection: 'back', windowed: true }),
      cancelled,
    ]);
    if (attempt !== generation || result === null) return;
    candidate = pairingDestination(result.content);
    document.querySelector('#pair-host').textContent = candidate.host;
    confirm.hidden = false;
  } catch (cause) {
    if (attempt === generation) error.textContent = cause instanceof Error ? cause.message : '扫码未完成，请重试。';
  } finally {
    if (attempt === generation) { settleCancel = null; restoreLauncher(); }
  }
});

cancelScan.addEventListener('click', async () => {
  const attempt = generation;
  const settle = settleCancel;
  cancelScan.disabled = true;
  try {
    await window.__TAURI__?.core?.invoke('plugin:barcode-scanner|cancel');
    if (attempt === generation) {
      // Plugin 2.4.6 can destroy its saved invocation without rejecting scan.
      // Settle our operation independently and fence any late native result.
      generation += 1;
      settleCancel = null;
      settle?.();
      restoreLauncher();
    }
  }
  catch { error.textContent = '请使用返回键退出扫码。'; }
  finally { cancelScan.disabled = false; }
});

document.querySelector('#pair-connect').addEventListener('click', () => {
  if (candidate === null) return;
  try { localStorage.setItem('neige-calm-server', candidate.origin); }
  catch { /* Pairing works even when remembering the origin is unavailable. */ }
  location.assign(candidate.url);
});
document.querySelector('#pair-cancel').addEventListener('click', () => {
  candidate = null;
  confirm.hidden = true;
});
