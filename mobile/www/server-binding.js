export async function bindServer(origin) {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) throw new Error('请在 Neige 安卓 App 中连接。');
  let result;
  try { result = await invoke('plugin:bundled-frontend|bind_server', { origin }); }
  catch (cause) { throw new Error(typeof cause === 'string' ? cause : cause?.message ?? '无法准备本地界面，请重试。'); }
  if (result?.origin !== origin) throw new Error('服务器地址确认失败，请重试。');
}
