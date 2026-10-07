"""ACP spy peer. No shell tools run; logs contain no environment or credential values."""
import json
import os
import pathlib
import subprocess
import sys
import uuid
import time
import hashlib

root = pathlib.Path(os.environ['ACP_FIXTURE_ROOT'])
with (root / 'environment.jsonl').open('a') as output:
    presence = {name: name in os.environ for name in [
        'NEIGE_MCP_DAEMON_TOKEN', 'NEIGE_MCP_TOKEN', 'NEIGE_MCP_SOCKET', 'ACP_AMBIENT_SENTINEL']}
    presence['readiness'] = os.environ['NEIGE_ACP_PLANNER'].endswith(':readiness')
    output.write(json.dumps(presence) + '\n')
current, pending, permission = None, None, None
model, effort = 'fixture/model-a', 'normal'

def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)

def result(request, value):
    emit({'jsonrpc': '2.0', 'id': request['id'], 'result': value})

def config():
    return {'configOptions': [
        {'id': 'declared-model-key', 'type': 'select', 'category': 'model', 'currentValue': model,
         'options': [{'group': 'Fixture', 'options': [
             {'value': 'fixture/model-a', 'name': 'Fixture A'}, {'value': 'fixture/model-b', 'name': 'Fixture B'}]}]},
        {'id': 'declared-effort-key', 'type': 'select', 'category': 'thought_level', 'currentValue': effort,
         'options': [{'value': 'normal', 'name': 'Normal'}, {'value': 'deep', 'name': 'Deep'}]}]}

def update(value):
    emit({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': current, 'update': value}})

def finish(request, text, reason='end_turn'):
    update({'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': text}})
    result(request, {'stopReason': reason})

def native_path():
    return root / (current + '.json')

def check_mcp(servers):
    if servers and (root / 'scenario').read_text().strip() == 'cli':
        cli = pathlib.Path(servers[0]['command']).with_name('neige')
        help_result = subprocess.run([str(cli), '--help'], capture_output=True, text=True, timeout=10)
        status = subprocess.run([str(cli), 'track', 'status', '--json'], capture_output=True, text=True, timeout=10)
        expected = {entry['name']: entry['value'] for entry in servers[0]['env']}
        fingerprint = hashlib.sha256(os.environ.get('NEIGE_MCP_TOKEN', '').encode()).hexdigest()
        previous = root / 'cli-token-fingerprint'
        rotated = previous.exists() and previous.read_text() != fingerprint
        previous.write_text(fingerprint)
        record = {'help_exit': help_result.returncode, 'status_exit': status.returncode,
                  'status': json.loads(status.stdout) if status.returncode == 0 else None,
                  'matches_mcp_context': all(os.environ.get(key) == value for key, value in expected.items()),
                  'token_rotated': rotated}
        with (root / 'cli-results.jsonl').open('a') as output:
            output.write(json.dumps(record) + '\n')
    if not servers or (root / 'scenario').read_text().strip() != 'mcp':
        return
    server = servers[0]
    env = dict(os.environ)
    env.update({entry['name']: entry['value'] for entry in server['env']})
    child = subprocess.Popen([server['command'], *server['args']], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env, text=True)
    try:
        child.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {
            'protocolVersion': '2024-11-05', 'capabilities': {}, 'clientInfo': {'name': 'fixture', 'version': '1'}}}) + '\n')
        child.stdin.flush()
        (root / 'mcp-reply.json').write_text(child.stdout.readline())
    finally:
        child.stdin.close()
        child.wait(timeout=5)

for line in sys.stdin:
    request = json.loads(line)
    logged = json.loads(json.dumps(request))
    for server in logged.get('params', {}).get('mcpServers', []):
        for variable in server.get('env', []):
            variable['value'] = '[redacted]'
        for header in server.get('headers', []):
            header['value'] = '[redacted]'
    with (root / 'requests.jsonl').open('a') as output:
        output.write(json.dumps(logged) + '\n')
    method, params = request.get('method'), request.get('params', {})
    if method == 'initialize':
        wrong = (root / 'scenario').read_text().strip() == 'wrong-identity' and not os.environ['NEIGE_ACP_PLANNER'].endswith(':readiness')
        if wrong:
            (root / 'wrong-identity.json').write_text(json.dumps({
                'token_present': 'NEIGE_MCP_TOKEN' in os.environ,
                'socket_present': 'NEIGE_MCP_SOCKET' in os.environ}))
        result(request, {'protocolVersion': 1, 'agentCapabilities': {'loadSession': True},
                         'agentInfo': {'name': 'Fixture ACP', 'version': 'wrong' if wrong else '1'}})
        if (root / 'scenario').read_text().strip() == 'checkpoint' and not os.environ['NEIGE_ACP_PLANNER'].endswith(':readiness'):
            (root / 'setup-checkpoint').touch()
            while not (root / 'release-setup').exists():
                time.sleep(0.01)
    elif method == 'session/new':
        current = 'native_' + uuid.uuid4().hex
        native_path().write_text(json.dumps({'inputs': [], 'cwd': params['cwd'], 'model': model, 'effort': effort}))
        check_mcp(params['mcpServers'])
        result(request, {'sessionId': current, **config()})
    elif method == 'session/load':
        current = params['sessionId']
        state = json.loads(native_path().read_text())
        assert state['cwd'] == params['cwd']
        model, effort = state['model'], state['effort']
        for text in state['inputs']:
            update({'sessionUpdate': 'user_message_chunk', 'content': {'type': 'text', 'text': text}})
            update({'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'historic reply'}})
        check_mcp(params['mcpServers'])
        result(request, config())
    elif method == 'session/set_config_option':
        if params['configId'] == 'declared-model-key':
            model = params['value']
        elif params['configId'] == 'declared-effort-key':
            effort = params['value']
        else:
            raise AssertionError('use the declared config key')
        state = json.loads(native_path().read_text())
        state.update(model=model, effort=effort)
        native_path().write_text(json.dumps(state))
        result(request, config())
    elif method == 'session/prompt':
        assert params['sessionId'] == current
        state = json.loads(native_path().read_text())
        texts = [part['text'] for part in params['prompt']]
        state['inputs'].extend(texts)
        native_path().write_text(json.dumps(state))
        scenario = (root / 'scenario').read_text().strip()
        if scenario == 'settlement':
            (root / 'before-settlement').touch()
            while not (root / 'release-settlement').exists():
                time.sleep(0.01)
        if scenario in ['lost', 'checkpoint']:
            os._exit(0)
        if scenario == 'permission':
            pending, permission = request, 'native-permission'
            emit({'jsonrpc': '2.0', 'id': permission, 'method': 'session/request_permission', 'params': {
                'sessionId': current, 'toolCall': {'toolCallId': 'one', 'title': 'Fixture operation'},
                'options': [{'optionId': 'yes', 'name': 'Allow', 'kind': 'allow_once'}]}})
        elif scenario == 'hold':
            pending = request
        else:
            update({'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'before operation'}})
            update({'sessionUpdate': 'tool_call', 'toolCallId': '1', 'title': 'Fixture output', 'status': 'in_progress'})
            update({'sessionUpdate': 'tool_call_update', 'toolCallId': '1', 'status': 'completed',
                    'content': [{'type': 'content', 'content': {'type': 'text', 'text': 'native tool output'}}]})
            finish(request, 'reply: ' + texts[-1])
    elif method == 'session/cancel' and pending:
        finish(pending, 'cancelled', 'cancelled')
        pending = None
    elif method is None and permission and request.get('id') == permission:
        (root / 'permission-reply.json').write_text(json.dumps(request))
        assert request['result']['outcome'] == {'outcome': 'cancelled'}, 'permission policy is never'
        finish(pending, 'permission declined', 'cancelled')
        pending = permission = None
    elif 'id' in request:
        emit({'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32601, 'message': 'unknown method'}})
