export function pairingDestination(input) {
  if (typeof input !== 'string' || input.length > 1024) throw new Error('这不是有效的 Neige 配对码。');
  let url;
  try { url = new URL(input); }
  catch { throw new Error('这不是有效的 Neige 配对码。'); }
  if (url.protocol !== 'https:' || url.username || url.password || url.search ||
      url.pathname !== '/mobile/pair' || !/^#v1\.[a-f0-9]{64}$/.test(url.hash) ||
      ['localhost', '127.0.0.1', '[::1]', 'tauri.localhost'].includes(url.hostname)) {
    throw new Error('请扫描网页设置中生成的 HTTPS 配对码。');
  }
  return { url: url.href, origin: url.origin, host: url.host };
}
