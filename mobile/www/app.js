import { nextUrl } from './server-url.js';
import { defaultServer } from './server-config.js';

const storageKey = 'neige-calm-server';
const form = document.querySelector('form');
const address = document.querySelector('#server');
const error = document.querySelector('#error');
const remember = document.querySelector('#remember');

try {
  address.value = localStorage.getItem(storageKey) ?? defaultServer;
} catch {
  remember.checked = false;
  remember.disabled = true;
  address.value = defaultServer;
}

form.addEventListener('submit', (event) => {
  event.preventDefault();
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
  try {
    if (remember.checked) localStorage.setItem(storageKey, new URL(destination).origin);
    else localStorage.removeItem(storageKey);
  } catch {
    // Storage is optional; the explicitly requested navigation still works.
  }
  // Top-level navigation preserves the server's normal cookie and WebSocket origin.
  window.location.assign(destination);
});
