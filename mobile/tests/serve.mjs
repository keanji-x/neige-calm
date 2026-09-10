import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';

const config = JSON.parse(await readFile(new URL('../src-tauri/tauri.conf.json', import.meta.url)));
const assets = new Map([
  ['/', ['index.html', 'text/html']],
  ['/app.js', ['app.js', 'text/javascript']],
  ['/scanner.js', ['scanner.js', 'text/javascript']],
  ['/pairing-url.js', ['pairing-url.js', 'text/javascript']],
  ['/server-url.js', ['server-url.js', 'text/javascript']],
  ['/server-binding.js', ['server-binding.js', 'text/javascript']],
  ['/server-config.js', ['server-config.js', 'text/javascript']],
  ['/style.css', ['style.css', 'text/css']],
  ['/neige-mark.svg', ['neige-mark.svg', 'image/svg+xml']],
]);

createServer(async (request, response) => {
  const asset = assets.get(request.url);
  if (!asset) { response.writeHead(404).end(); return; }
  response.writeHead(200, {
    'Content-Type': asset[1],
    'Content-Security-Policy': config.app.security.csp,
  });
  response.end(await readFile(new URL(`../www/${asset[0]}`, import.meta.url)));
}).listen(5197, '127.0.0.1');
