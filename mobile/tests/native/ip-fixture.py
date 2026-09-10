"""Loopback-only HTTP peers for the Android direct-route boundary smoke."""
import http.server
import json
import threading
import time

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
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
            body = json.dumps({'apiVersion': '6', 'kernelVersion': 'test-peer', 'webCompatVersion': 26, 'minWebCompatVersion': 26}).encode()
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

for port in (5413, 5414, 5415):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
threading.Event().wait()
