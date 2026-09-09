import { httpOrigins } from './server-config.js';

export function nextUrl(input) {
  let url;
  try {
    url = new URL(input.trim());
  } catch {
    throw new Error('请输入完整的服务器地址，例如 https://calm.example.com');
  }
  if (url.protocol !== 'https:' && !httpOrigins.includes(url.origin)) {
    throw new Error('请使用 HTTPS 地址，或此安装包配置的本地服务器地址。');
  }
  if (url.username || url.password || url.search || url.hash) {
    throw new Error('地址中不能包含账号、密码、查询参数或锚点。');
  }
  if (!['/', '/next', '/next/'].includes(url.pathname)) {
    throw new Error('请填写服务器首页地址，或以 /next/ 结尾的地址。');
  }
  if (!httpOrigins.includes(url.origin) && ['tauri.localhost', 'localhost', '127.0.0.1', '[::1]'].includes(url.hostname)) {
    throw new Error('请填写手机能访问的服务器地址，不能使用本机回环地址。');
  }
  return `${url.origin}/next/`;
}
