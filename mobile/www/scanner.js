import { pairingDestination } from './pairing-url.js';

const scan = document.querySelector('#scan');
const cancelScan = document.querySelector('#cancel-scan');
const confirm = document.querySelector('#pair-confirm');
const error = document.querySelector('#scan-error');
let candidate = null;

scan.addEventListener('click', async () => {
  const invoke = window.__TAURI__?.core?.invoke;
  error.textContent = '';
  candidate = null;
  confirm.hidden = true;
  if (!invoke) { error.textContent = '请在 Neige 安卓 App 中使用扫码。'; return; }
  scan.disabled = true;
  try {
    const permission = await invoke('plugin:barcode-scanner|request_permissions');
    if (permission.camera !== 'granted') throw new Error('请允许相机权限后重试。');
    cancelScan.hidden = false;
    // These are the official plugin's guest arguments; no remote origin is
    // granted the native capability used by this packaged launcher.
    const result = await invoke('plugin:barcode-scanner|scan', { formats: ['QR_CODE'], cameraDirection: 'back', windowed: false });
    candidate = pairingDestination(result.content);
    document.querySelector('#pair-host').textContent = candidate.host;
    confirm.hidden = false;
  } catch (cause) {
    error.textContent = cause instanceof Error ? cause.message : '扫码未完成，请重试。';
  } finally {
    scan.disabled = false;
    cancelScan.hidden = true;
  }
});

cancelScan.addEventListener('click', async () => {
  try { await window.__TAURI__?.core?.invoke('plugin:barcode-scanner|cancel'); }
  catch { error.textContent = '请使用返回键退出扫码。'; }
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
