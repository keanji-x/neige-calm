"""Loopback-only HTTP peers for the Android direct-route boundary smoke."""
import http.server
import json
import threading
import time

stats = {'initial': 0, 'sink': 0}

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/stats':
            body = json.dumps(stats).encode()
            self.send_response(200); self.end_headers(); self.wfile.write(body); return
        if self.path == '/reset':
            stats.update(initial=0, sink=0)
            self.send_response(200); self.end_headers(); return
        if self.server.server_port == 5416 or self.path == '/sink':
            stats['sink'] += 1
            self.send_response(200); self.end_headers(); self.wfile.write(b'not an image'); return
        if self.path == '/redirect-resource':
            stats['initial'] += 1
            self.send_response(302)
            self.send_header('Location', 'http://10.0.2.2:5416/sink')
            self.end_headers(); return
        if self.path == '/redirect-document':
            self.send_response(200); self.send_header('Content-Type', 'text/html'); self.end_headers()
            self.wfile.write(b'<html><body><h1>Redirect test</h1><img src="/redirect-resource"></body></html>')
            return
        if self.server.server_port == 5414:
            time.sleep(8)
            return
        if self.server.server_port == 5415:
            self.send_response(302)
            self.send_header('Location', 'http://10.0.2.2:5413/api/version')
            self.end_headers()
            return
        if self.path == '/api/version':
            if self.headers.get('Cookie') or self.headers.get('Authorization'):
                self.send_error(400, 'Probe must not send credentials')
                return
            body = json.dumps({'areaCreateIdempotency': True, 'apiVersion': '6', 'kernelVersion': '0.1.0', 'webCompatVersion': 26, 'minWebCompatVersion': 26, 'syncEventVersion': 20, 'mcpProtocolVersion': '2024-11-05', 'pluginMcpProtocolVersion': '2025-11-25', 'supervisorControlVersion': 1, 'buildSha': '0' * 40, 'dbInstanceId': '00000000-0000-4000-8000-000000000001'}).encode()
            self.send_response(200)
        elif self.path == '/api/auth/whoami':
            body = b'{"error":"unauthorized"}'
            self.send_response(401)
        else:
            body = b'Frontend resources must come from the APK'
            self.send_response(500)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *_):
        pass

for port in (5413, 5414, 5415, 5416):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
threading.Event().wait()
