"""Exercise Stop during the real spy peer's native writes, without corrupting files."""
import json
import os
import pathlib
import select
import socket
import subprocess
import sys
import tempfile
import unittest

AGENT = pathlib.Path(__file__).with_name('agent.py')
# Instrument the actual pathlib write in a child running the real peer entry point.
# The first write creates the session; stop the next update after opening its file.
INTERRUPT = '''
import pathlib, runpy, socket, sys
barrier = socket.socket(fileno=int(sys.argv[2]))
original_open = pathlib.Path.open
writes = 0
def opened(path, mode='r', *args, **kwargs):
    global writes
    output = original_open(path, mode, *args, **kwargs)
    if mode == 'w' and path.name.startswith('native_'):
        writes += 1
        if writes == 2:
            barrier.sendall(b'!')
            barrier.recv(1)
    return output
pathlib.Path.open = opened
runpy.run_path(sys.argv[1], run_name='__main__')
'''


class NativePersistence(unittest.TestCase):
    def request(self, child, number, method, params):
        child.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': number,
                                     'method': method, 'params': params}) + '\n')
        child.stdin.flush()

    def response(self, child, number):
        # Unbuffered stdout avoids hiding another frame from select in TextIO buffers.
        while True:
            self.assertTrue(select.select([child.stdout], [], [], 10)[0], 'peer response')
            line = child.stdout.readline()
            if not line:
                self.fail('peer exited: ' + child.stderr.read().decode())
            frame = json.loads(line)
            if frame.get('id') == number:
                self.assertIn('result', frame)
                return frame['result']

    def spawn(self, root, barrier=None):
        command = [sys.executable, '-u', str(AGENT)]
        passed = ()
        if barrier is not None:
            command = [sys.executable, '-u', '-c', INTERRUPT, str(AGENT), str(barrier.fileno())]
            passed = (barrier.fileno(),)
        child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, bufsize=0, pass_fds=passed,
                                 env={**os.environ, 'ACP_FIXTURE_ROOT': str(root)})
        # Keep stdin convenient, but leave stdout as unbuffered bytes.
        import io
        child.stdin = io.TextIOWrapper(child.stdin, write_through=True)
        self.addCleanup(self.stop, child)
        self.request(child, 1, 'initialize', {})
        self.response(child, 1)
        return child

    @staticmethod
    def stop(child):
        if child.poll() is None:
            child.kill()
        child.wait(timeout=10)
        child.stdin.close()
        child.stdout.close()
        child.stderr.close()

    def interrupted_update(self, method, params):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / 'scenario').write_text('reply')
            controller, peer = socket.socketpair()
            with controller, peer:
                child = self.spawn(root, peer)
                self.request(child, 2, 'session/new', {'cwd': directory, 'mcpServers': []})
                native = self.response(child, 2)['sessionId']
                path = root / (native + '.json')
                before = path.read_bytes()
                self.request(child, 3, method, {'sessionId': native, **params})
                controller.settimeout(10)
                self.assertEqual(controller.recv(1), b'!')
                # Kill the writer at its actual open/truncate boundary, never write the file here.
                child.kill()
                child.wait(timeout=10)
                successor = self.spawn(root)
                self.request(successor, 2, 'session/resume', {
                    'sessionId': native, 'cwd': directory, 'mcpServers': []})
                self.response(successor, 2)
                self.assertEqual(path.read_bytes(), before)

    def test_prompt_interrupted_write_remains_resumable(self):
        self.interrupted_update('session/prompt', {'prompt': [{'type': 'text', 'text': 'hello'}]})

    def test_config_interrupted_write_remains_resumable(self):
        self.interrupted_update('session/set_config_option', {
            'configId': 'declared-model-key', 'value': 'fixture/model-b'})


if __name__ == '__main__':
    unittest.main()
