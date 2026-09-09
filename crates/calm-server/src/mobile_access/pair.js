const fragment = location.hash;
history.replaceState(null, '', location.pathname);
const status = document.querySelector('#status');
const error = document.querySelector('#error');
const button = document.querySelector('#connect');
document.querySelector('#server').textContent = location.host;
const valid = /^#v1\.([a-f0-9]{64})$/.exec(fragment);
let ticket = valid?.[1];
let stopped = false;
addEventListener('pagehide', () => { stopped = true; });

async function post(path, body) {
  return fetch(path, {
    method: 'POST', credentials: 'same-origin', cache: 'no-store',
    headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
    signal: AbortSignal.timeout(10_000),
  });
}

if (!ticket) {
  status.textContent = '此二维码无效或已过期，请重新扫码。';
} else {
  status.textContent = '确认这是你要连接的服务器。';
  button.hidden = false;
}

button.addEventListener('click', async () => {
  button.disabled = true;
  error.textContent = '';
  try {
    const response = await post('/api/mobile/pairings/claim', { ticket, deviceName: 'Android phone' });
    ticket = undefined;
    if (!response.ok) throw new Error('配对码已使用或过期，请重新扫码。');
    const claim = await response.json();
    document.querySelector('#code').textContent = claim.verificationCode;
    status.textContent = '等待电脑网页确认…';
    button.hidden = true;
    const deadline = Date.now() + 180_000;
    while (!stopped && Date.now() < deadline) {
      const result = await post('/api/mobile/pairings/redeem', { id: claim.id, secret: claim.secret });
      if (result.status === 204) { location.replace('/next/'); return; }
      if (result.status !== 202) throw new Error('配对已失效，请重新扫码。');
      await new Promise((resolve) => setTimeout(resolve, 1500));
    }
    if (!stopped) throw new Error('等待超时，请重新扫码。');
  } catch (cause) {
    status.textContent = '连接未完成';
    error.textContent = cause instanceof Error ? cause.message : '连接失败，请重新扫码。';
  }
});
