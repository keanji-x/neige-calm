import { nextUrl } from './server-url.js';
import { defaultServer } from './server-config.js';
import { bindServer } from './server-binding.js';

const storageKey = 'neige-calm-server';
const form = document.querySelector('form');
const address = document.querySelector('#server');
const error = document.querySelector('#error');
const remember = document.querySelector('#remember');
const connect = form.querySelector('button[type="submit"]');
let connecting = false;

try {
  address.value = localStorage.getItem(storageKey) ?? defaultServer;
} catch {
  remember.checked = false;
  remember.disabled = true;
  address.value = defaultServer;
}

form.addEventListener('submit', async (event) => {
  event.preventDefault();
  if (connecting) return;
  error.textContent = '';
  address.removeAttribute('aria-invalid');
  let destination;
  try {
    destination = nextUrl(address.value);
  } catch (cause) {
    error.textContent = cause.message;
    address.setAttribute('aria-invalid', 'true');
    address.focus();
    return;
  }
  connecting = true;
  connect.disabled = true;
  try { await bindServer(new URL(destination).origin); }
  catch (cause) {
    error.textContent = cause.message;
    connecting = false;
    connect.disabled = false;
    return;
  }
  try {
    if (remember.checked) localStorage.setItem(storageKey, new URL(destination).origin);
    else localStorage.removeItem(storageKey);
  } catch {
    // Storage is optional; the explicitly requested navigation still works.
  }
  // Top-level navigation preserves the server's normal cookie and WebSocket origin.
  window.location.assign(destination);
});
