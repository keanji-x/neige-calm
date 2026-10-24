"""ACP spy peer. No shell tools run; logs contain no environment or credential values.

Its registration's env shapes `initialize`: ACP_FIXTURE_IDENTITY `agent_info` (default: agentInfo
named OpenCode) or `meta` (no agentInfo; `_meta.agentVersion`, as Grok Build 1.0.62 reports),
ACP_FIXTURE_VERSION (default 2.1.3), ACP_FIXTURE_AUTH_METHODS (comma-separated `authMethods`
ids; default none), and ACP_FIXTURE_AUTHENTICATE `ok` (default), `error` or `silent` (never
answers, as signed-out Grok Build 1.0.62). ACP_FIXTURE_VENDOR `xai` adds what Grok Build 1.0.62
sends beside ACP: `_x.ai/*` notifications around setup and every turn, and its `initialize`
`_meta` and capabilities (no image prompts). The `grok-mcp` scenario asks before an MCP tool as Grok
Build 1.0.62 does (`use_tool` titled `<server>__<tool>`, kind `other`); allowed once, it calls the
tool and finishes, and an answer `cancelled` or rejected ends the turn `cancelled`, as Grok's does.
Launched with `--always-approve`, it calls the tools without asking. `grok-mcp-unlisted` asks for a
tool the server does not list a Planner, and `grok-shell` for a shell command; each ends its turn
`cancelled` on any answer but an allow."""
import json
import os
import pathlib
import subprocess
import sys
import uuid
import time
import hashlib
import socket

root = pathlib.Path(os.environ['ACP_FIXTURE_ROOT'])
# Opt-in cold-recovery observer: one append-only, credential-free stream. The
# workspace is supplied by the case, never inferred from a native session ID.
cold_workspace = root / 'cold-observer-workspace'
def cold_trace(event, request=None, response=None):
    if not cold_workspace.exists():
        return
    record = {'schema': 1, 'pid': os.getpid(), 'event': event,
              'credential': any(name in os.environ for name in [
                  'NEIGE_MCP_TOKEN', 'NEIGE_MCP_SOCKET', 'NEIGE_MCP_DAEMON_TOKEN'])}
    if request is not None:
        record.update(id=request.get('id'), method=request.get('method'))
        params = request.get('params', {})
        if request.get('method') == 'session/new':
            record.update(mcp_empty=params.get('mcpServers') == [],
                          accepted_cwd=params.get('cwd') == cold_workspace.read_text())
        if request.get('method') == 'session/close':
            record['session_id'] = params.get('sessionId')
    if response is not None:
        record['session_id'] = response.get('sessionId')
    with (root / 'cold-trace.jsonl').open('a') as output:
        output.write(json.dumps(record) + '\n')

cold_trace('start')
with (root / 'environment.jsonl').open('a') as output:
    presence = {name: name in os.environ for name in [
        'NEIGE_MCP_DAEMON_TOKEN', 'NEIGE_MCP_TOKEN', 'NEIGE_MCP_SOCKET', 'ACP_AMBIENT_SENTINEL']}
    presence['_pid'] = os.getpid()
    presence['probe'] = {key: hashlib.sha256(os.environ.get(key, '').encode()).hexdigest() for key in os.environ.get('ACP_FIXTURE_PROBE_KEYS', '').split(',') if key}
    presence['opencode_config_content'] = os.environ.get('OPENCODE_CONFIG_CONTENT')
    output.write(json.dumps(presence) + '\n')
with (root / 'argv.jsonl').open('a') as output:
    output.write(json.dumps(sys.argv[1:]) + '\n')
current, pending, permission, cancel_seen = None, None, None, False
mcp_servers = []
xai = os.environ.get('ACP_FIXTURE_VENDOR') == 'xai'
always_approve = '--always-approve' in sys.argv
model, effort = 'fixture/model-a', 'normal'
compaction = False

def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)

def result(request, value):
    if request.get('method') == 'session/new':
        cold_trace('result', request, value)
    emit({'jsonrpc': '2.0', 'id': request['id'], 'result': value})

def hold_catalog():
    helper = subprocess.Popen(['/bin/sleep', '300'], stdin=subprocess.DEVNULL,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    (root / 'catalog-helper').write_text(str(helper.pid))
    while True:
        time.sleep(0.01)

def config():
    return {'configOptions': [
        {'id': 'declared-model-key', 'type': 'select', 'category': 'model', 'currentValue': model,
         'options': [{'group': 'Fixture', 'options': [
             {'value': 'fixture/model-a', 'name': 'Fixture A'}, {'value': 'fixture/model-b', 'name': 'Fixture B'}]}]},
        {'id': 'declared-effort-key', 'type': 'select', 'category': 'thought_level', 'currentValue': effort,
         'options': [{'value': 'normal', 'name': 'Normal'}, {'value': 'deep', 'name': 'Deep'}]}]}

def vendor(method, params=None):
    # Grok Build's own notifications; an ACP client ignores methods it does not know.
    if xai:
        emit({'jsonrpc': '2.0', 'method': '_x.ai/' + method, 'params': params or {'sessionId': current}})

def call_tools(server, calls):
    """Call `calls` in order on one connection to `server`'s stdio command, as the agent's MCP
    client would; the replies in order."""
    env = dict(os.environ)
    env.update({entry['name']: entry['value'] for entry in server['env']})
    child = subprocess.Popen([server['command'], *server['args']], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env, text=True)
    try:
        child.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {
            'protocolVersion': '2024-11-05', 'capabilities': {}, 'clientInfo': {'name': 'fixture', 'version': '1'}}}) + '\n')
        child.stdin.flush()
        child.stdout.readline()
        replies = []
        for number, (name, arguments) in enumerate(calls, start=2):
            child.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': number, 'method': 'tools/call',
                                          'params': {'name': name, 'arguments': arguments}}) + '\n')
            child.stdin.flush()
            replies.append(json.loads(child.stdout.readline()))
        return replies
    finally:
        child.stdin.close()
        child.wait(timeout=5)

def ask_mcp_tool(request_id, tool='neige_report_commit'):
    # The shape Grok Build 1.0.62 sends before `use_tool` calls a tool of a client-supplied server.
    title = mcp_servers[0]['name'] + '__' + tool
    emit({'jsonrpc': '2.0', 'id': request_id, 'method': 'session/request_permission', 'params': {
        'sessionId': current,
        'toolCall': {'toolCallId': 'call-grok-0', 'kind': 'other', 'title': title,
                     'rawInput': {'variant': 'UseTool', 'tool_name': title, 'tool_input': {}},
                     '_meta': {'x.ai/tool': {'version': 1, 'name': 'use_tool', 'kind': 'use_tool',
                                             'namespace': 'grok_build', 'label': 'Use Tool', 'read_only': False}}},
        'options': [{'optionId': 'always-allow', 'name': 'always allow', 'kind': 'allow_always'},
                    {'optionId': 'allow-once', 'name': 'allow once', 'kind': 'allow_once'},
                    {'optionId': 'reject-once', 'name': 'reject once', 'kind': 'reject_once'}]}})

def ask_shell(request_id):
    # The shape Grok Build 1.0.62 sends before `run_terminal_command` runs a command that writes.
    command = 'echo one > one.txt'
    emit({'jsonrpc': '2.0', 'id': request_id, 'method': 'session/request_permission', 'params': {
        'sessionId': current,
        'toolCall': {'toolCallId': 'call-grok-1', 'kind': 'execute', 'title': 'Execute `' + command + '`',
                     'rawInput': {'variant': 'Bash', 'command': command, 'is_background': False},
                     '_meta': {'x.ai/tool': {'version': 1, 'name': 'run_terminal_command', 'kind': 'execute',
                                             'namespace': 'grok_build', 'label': 'Run Command', 'read_only': False}}},
        'options': [{'optionId': 'always-allow', 'name': "Yes, and don't ask again for bash commands", 'kind': 'allow_always'},
                    {'optionId': 'allow-once', 'name': 'Yes, proceed', 'kind': 'allow_once'},
                    {'optionId': 'reject-once', 'name': 'No, and tell Grok what to do differently', 'kind': 'reject_once'}]}})

def write_report(request):
    replies = call_tools(mcp_servers[0], [
        ('neige_report_read', {'with_markers': True}),
        ('neige_report_commit', {'message': 'Grok prose', 'ops': [
            {'op': 'upsert', 'kind': 'prose', 'markdown': '# Grok note\n\nWritten through use_tool.\n'}]})])
    (root / 'grok-mcp-result.json').write_text(json.dumps(replies))
    vendor('session/prompt_complete')
    finish(request, 'tools answered')

def update(value):
    emit({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': current, 'update': value}})

def attach_updates():
    scenario = (root / 'scenario').read_text().strip()
    if (xai and not scenario.startswith('grok-compact-')) or scenario == 'compact-undeclared':
        return
    update({'sessionUpdate': 'available_commands_update', 'availableCommands': [{'name': 'compact', 'description': 'Compact the session'}]})
    if not xai:
        update({'sessionUpdate': 'config_option_update', **config()})

def finish(request, text, reason='end_turn'):
    update({'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': text}})
    result(request, {'stopReason': reason})

def permission_log(entry):
    with (root / 'permission-log.jsonl').open('a') as output:
        output.write(json.dumps(entry) + '\n')

def ask_permission(request_id):
    # The OpenCode 2.1.3 correlated shell permission shape.
    emit({'jsonrpc': '2.0', 'id': request_id, 'method': 'session/request_permission', 'params': {
        'sessionId': current,
        'toolCall': {'toolCallId': 'call_1', 'name': 'shell', 'title': 'echo one > one.txt', 'kind': 'execute',
                     'status': 'pending', 'locations': [], 'rawInput': {'command': 'echo one > one.txt'}},
        'options': [{'optionId': 'once', 'kind': 'allow_once', 'name': 'Allow once'},
                    {'optionId': 'always', 'kind': 'allow_always', 'name': 'Always allow'},
                    {'optionId': 'reject', 'kind': 'reject_once', 'name': 'Reject'}]}})

def native_path():
    return root / (current + '.json')

def write_native(state):
    # Stop may interrupt a prompt write; resume must still see a complete snapshot.
    path = native_path()
    pending = path.with_suffix('.pending')
    pending.write_text(json.dumps(state))
    pending.replace(path)

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
    if not servers or (root / 'scenario').read_text().strip() not in ['mcp', 'assistant-mcp', 'assistant-canonical']:
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
        if (root / 'scenario').read_text().strip() in ['assistant-mcp', 'assistant-canonical']:
            replies = []
            for number, name, arguments in [
                (2, 'neige_report_read', {'with_markers': True}),
                (3, 'neige_report_commit', {'message': 'Assistant prose', 'ops': [
                    {'op': 'upsert', 'kind': 'prose', 'markdown': '# Assistant note\n\nNative ACP prose.\n'}]}),
                (4, 'neige_task_accept', {}),
                (5, 'neige_terminal_open', {}),
                (6, 'neige_track_close', {})]:
                (root / 'assistant-mcp-progress').write_text(name)
                child.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': number, 'method': 'tools/call',
                    'params': {'name': name, 'arguments': arguments}}) + '\n')
                child.stdin.flush()
                replies.append(json.loads(child.stdout.readline()))
                (root / 'assistant-mcp-results.json').write_text(json.dumps(replies))
            (root / 'assistant-mcp-results.json').write_text(json.dumps(replies))
    finally:
        child.stdin.close()
        child.wait(timeout=5)

for line in sys.stdin:
    request = json.loads(line)
    cold_trace('request', request)
    logged = json.loads(json.dumps(request))
    logged['_pid'] = os.getpid()
    for server in logged.get('params', {}).get('mcpServers', []):
        for variable in server.get('env', []):
            variable['value'] = '[redacted]'
        for header in server.get('headers', []):
            header['value'] = '[redacted]'
    with (root / 'requests.jsonl').open('a') as output:
        output.write(json.dumps(logged) + '\n')
    method, params = request.get('method'), request.get('params', {})
    if method == 'initialize':
        compaction = params.get('clientCapabilities', {}).get('session', {}).get('compaction') is not None
        wrong = (root / 'scenario').read_text().strip() == 'wrong-identity' and 'NEIGE_MCP_TOKEN' in os.environ
        if wrong:
            (root / 'wrong-identity.json').write_text(json.dumps({
                'token_present': 'NEIGE_MCP_TOKEN' in os.environ,
                'socket_present': 'NEIGE_MCP_SOCKET' in os.environ}))
        if (root / 'scenario').read_text().strip() == 'catalog_cancel_initialize':
            hold_catalog()
        capabilities = {'loadSession': (root / 'scenario').read_text().strip() not in ['unsupported-load', 'resume-only']}
        if (root / 'scenario').read_text().strip() != 'catalog_no_close':
            capabilities['sessionCapabilities'] = {'close': {}}
        if not xai and (root / 'scenario').read_text().strip() not in ['unsupported-load', 'load-only', 'load-error', 'load-replay-lost']:
            capabilities.setdefault('sessionCapabilities', {})['resume'] = {}
        version = 'wrong' if wrong else os.environ.get('ACP_FIXTURE_VERSION', '2.1.3')
        if xai:
            capabilities['promptCapabilities'] = {'image': False, 'audio': False, 'embeddedContext': True}
            capabilities['_meta'] = {'x.ai/fs_notify': True}
        initialized = {'protocolVersion': 1, 'agentCapabilities': capabilities}
        if os.environ.get('ACP_FIXTURE_IDENTITY', 'agent_info') == 'meta':
            initialized['_meta'] = {'agentVersion': version}
            if xai:
                initialized['_meta'].update({'defaultAuthMethodId': 'cached_token', 'grokShell': True})
        else:
            initialized['agentInfo'] = {'name': 'OpenCode', 'version': version}
        methods = [m for m in os.environ.get('ACP_FIXTURE_AUTH_METHODS', '').split(',') if m]
        if methods:
            initialized['authMethods'] = [{'id': m, 'name': m} for m in methods]
        result(request, initialized)
        if (root / 'scenario').read_text().strip() == 'checkpoint' and 'NEIGE_MCP_TOKEN' in os.environ:
            (root / 'setup-checkpoint').touch()
            while not (root / 'release-setup').exists():
                time.sleep(0.01)
    elif method == 'authenticate':
        behaviour = os.environ.get('ACP_FIXTURE_AUTHENTICATE', 'ok')
        if behaviour == 'ok':
            result(request, {})
        elif behaviour == 'error':
            emit({'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32000, 'message': 'Authentication required'}})
    elif method == 'session/new':
        if (root / 'scenario').read_text().strip() == 'catalog_cancel':
            hold_catalog()
        if (root / 'scenario').read_text().strip() == 'catalog_error':
            emit({'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32603, 'message': 'catalog setup failed'}})
            continue
        current = 'native_' + uuid.uuid4().hex
        native_path().write_text(json.dumps({'inputs': [], 'cwd': params['cwd'], 'model': model, 'effort': effort}))
        mcp_servers = params['mcpServers']
        vendor('mcp/servers_updated')
        vendor('session/setup')
        check_mcp(params['mcpServers'])
        if (root / 'scenario').read_text().strip() == 'closed-input':
            # Keep stdout/process alive but remove every reader before setup completes.
            os.close(0)
            result(request, {'sessionId': current, **config()})
            while True:
                time.sleep(0.01)
        attach_updates()
        result(request, {'sessionId': current, **config()})
    elif method == 'session/close':
        assert params['sessionId'] == current
        result(request, {})
    elif method in ['session/load', 'session/resume']:
        if (root / 'scenario').read_text().strip() in ['load-error', 'resume-error']:
            emit({'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32602, 'message': 'cannot attach session'}})
            continue
        current = params['sessionId']
        state = json.loads(native_path().read_text())
        assert state['cwd'] == params['cwd']
        model, effort = state['model'], state['effort']
        for text in state['inputs'] if method == 'session/load' else []:
            update({'sessionUpdate': 'user_message_chunk', 'content': {'type': 'text', 'text': text}})
            update({'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'historic reply'}})
        mcp_servers = params['mcpServers']
        vendor('session/setup')
        attach_updates()
        if xai and (root / 'scenario').read_text().strip().startswith('grok-compact-'):
            vendor('session/update', {'sessionId': current, 'update': {
                'sessionUpdate': 'auto_compact_completed', 'tokens_before': 23450,
                'tokens_after': 23450, 'summary_preview': None}, '_meta': {'isReplay': True}})
            if (root / 'scenario').read_text().strip().startswith('grok-compact-pre-send'):
                import threading
                def pre_send_completion():
                    while not (root / 'grok-pre-send').exists():
                        time.sleep(0.001)
                    vendor('session_notification', {'sessionId': current, 'update': {
                        'sessionUpdate': 'auto_compact_completed', 'tokens_before': 23450,
                        'tokens_after': 23450}, '_meta': {'isReplay': False}})
                threading.Thread(target=pre_send_completion, daemon=True).start()

        check_mcp(params['mcpServers'])
        result(request, config())
        # Fixture-only barrier: setup has replied, but the next prompt is not read yet.
        if (root / 'prompt-consumption.sock').exists():
            with socket.socket(socket.AF_UNIX) as barrier:
                barrier.connect(str(root / 'prompt-consumption.sock'))
                assert barrier.recv(1) == b'!'
    elif method == 'session/set_config_option':
        if params['configId'] == 'declared-model-key':
            model = params['value']
        elif params['configId'] == 'declared-effort-key':
            effort = params['value']
        else:
            raise AssertionError('use the declared config key')
        state = json.loads(native_path().read_text())
        state.update(model=model, effort=effort)
        write_native(state)
        if not xai:
            update({'sessionUpdate': 'config_option_update', **config()})
        result(request, config())
    elif method == '_x.ai/compact_conversation':
        assert xai and not compaction, 'Grok does not negotiate standard compaction'
        assert params['session_id'] == current
        assert isinstance(params['user_context'], str) and params['user_context']
        scenario = (root / 'scenario').read_text().strip()
        def completed(session=current, replay=False, method='session_notification'):
            vendor(method, {'sessionId': session, 'update': {
                'sessionUpdate': 'auto_compact_completed', 'tokens_before': 23450,
                'tokens_after': 23450, 'summary_preview': None},
                '_meta': {'eventId': 'opaque-event', 'agentTimestampMs': 1792827612149,
                          'isReplay': replay}})
        if scenario == 'grok-compact-hung':
            completed()
            completed(replay=True)
            completed(replay=True, method='session/update')
        if scenario.endswith('-mixed'):
            # No notification, even a valid live event, substitutes for this RPC id.
            completed()
            completed(session='another-session')
            completed(replay=True)
            completed(replay=True, method='session/update')
            scenario = scenario.removesuffix('-mixed')
        if scenario == 'grok-compact-unknown':
            emit({'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32601, 'message': 'unknown ACP extension method'}})
            continue
        if scenario == 'grok-compact-hung':
            # Keep stdin, stdout and the process alive without answering.
            pending = request
            continue
        if scenario == 'grok-compact-death':
            os._exit(0)
        if scenario == 'grok-compact-replay':
            completed(replay=True)
        elif scenario == 'grok-compact-replay-method':
            completed(replay=True, method='session/update')
        elif scenario.startswith('grok-compact-wrong-session'):
            completed(session='another-session')
            if scenario.endswith('-then-valid'):
                completed()
        elif scenario == 'grok-compact-summary':
            vendor('session_notification', {'sessionId': current, 'update': {
                'sessionUpdate': 'session_summary_generated', 'session_summary': 'summary'}})
            vendor('session/update', {'sessionId': current, 'update': {
                'sessionUpdate': 'compaction_checkpoint'}})
        elif scenario == 'grok-compact-standard':
            update({'sessionUpdate': 'compaction_update', 'compactionId': 'standard', 'status': 'in_progress'})
            update({'sessionUpdate': 'compaction_update', 'compactionId': 'standard', 'status': 'completed'})
        elif scenario not in ['grok-compact-empty', 'grok-compact-failed', 'grok-compact-pre-send', 'grok-compact-pre-send-lost-response']:
            completed()
        if scenario == 'grok-compact-pre-send-lost-response':
            emit({'jsonrpc': '2.0', 'id': 900000000, 'result': {}})
            os._exit(0)
        if scenario == 'grok-compact-lost-response':
            os._exit(0)
        if scenario in ['grok-compact-failed', 'grok-compact-failed-live']:
            emit({'jsonrpc': '2.0', 'id': request['id'], 'error': {
                'code': -32603, 'message': 'compact failed', 'data': {'kind': 'compact_failed'}}})
        else:
            result(request, {'unexpected': True} if scenario == 'grok-compact-invalid-response' else {})
    elif method == 'session/prompt':
        assert params['sessionId'] == current
        state = json.loads(native_path().read_text())
        texts = [part['text'] for part in params['prompt']]
        state['inputs'].extend(texts)
        write_native(state)
        scenario = (root / 'scenario').read_text().strip()
        if texts == ['/compact']:
            assert compaction, 'standard compaction capability must be negotiated'
            if scenario.startswith('compact-review-'):
                def lifecycle(cid, status):
                    update({'sessionUpdate': 'compaction_update', 'compactionId': cid, 'status': status})
                if scenario == 'compact-review-auto-first':
                    lifecycle('auto', 'in_progress')
                    lifecycle('auto', 'completed')
                    lifecycle('manual', 'in_progress')
                    lifecycle('manual', 'failed')
                elif scenario == 'compact-review-replayed-id':
                    lifecycle('old-settled', 'completed')
                    lifecycle('old-settled', 'in_progress')
                    lifecycle('old-settled', 'completed')
                result(request, {'stopReason': 'end_turn'})
                continue
            # Successful maintenance has exactly one previously unseen lifecycle.
            native_id = 'compact_' + uuid.uuid4().hex
            update({'sessionUpdate': 'compaction_update', 'compactionId': native_id, 'status': 'in_progress'})
            update({'sessionUpdate': 'compaction_summary_chunk', 'compactionId': native_id, 'content': {'type': 'text', 'text': 'summary'}})
            if scenario == 'compact-death':
                os._exit(0)
            if scenario != 'compact-lost':
                status = scenario.removeprefix('compact-') if scenario in ['compact-failed', 'compact-cancelled'] else 'completed'
                update({'sessionUpdate': 'compaction_update', 'compactionId': 'auto-other' if scenario == 'compact-other-id' else native_id, 'status': status})
            if scenario == 'compact-no-response':
                os._exit(0)
            result(request, {'stopReason': 'end_turn'})
            continue
        if scenario == 'settlement':
            (root / 'before-settlement').touch()
            while not (root / 'release-settlement').exists():
                time.sleep(0.01)
        if scenario in ['lost', 'checkpoint', 'load-replay-lost']:
            os._exit(0)
        vendor('queue/changed')
        if scenario == 'grok-mcp' and always_approve:
            write_report(request)
        elif scenario in ['grok-mcp', 'grok-mcp-unlisted', 'grok-shell']:
            pending, permission, cancel_seen = request, 'grok-permission', False
            if scenario == 'grok-shell':
                ask_shell(permission)
            else:
                ask_mcp_tool(permission, 'neige_task_done' if scenario == 'grok-mcp-unlisted' else 'neige_report_commit')
        elif scenario in ['permission', 'assistant-canonical']:
            pending, permission = request, 'native-permission'
            emit({'jsonrpc': '2.0', 'id': permission, 'method': 'session/request_permission', 'params': {
                'sessionId': current, 'toolCall': {'toolCallId': 'one', 'title': 'Fixture operation'},
                'options': [{'optionId': 'yes', 'name': 'Allow', 'kind': 'allow_once'}]}})
        elif scenario in ['ask', 'ask-cancel', 'ask-exit', 'ask-settle']:
            pending, permission, cancel_seen = request, 0, False
            ask_permission(permission)
            if scenario == 'ask-exit':
                time.sleep(0.3)
                os._exit(0)
            if scenario == 'ask-settle':
                # The turn settles on its own while the request still waits; later answers are logged.
                time.sleep(0.3)
                finish(pending, 'settled')
                pending = None
        elif scenario in ['hold', 'server-drain']:
            pending = request
            if (root / 'prompt-consumption.sock').exists():
                update({'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'held prompt received'}})
            if scenario == 'server-drain':
                # 2.x standalone contract: inherited launch env, private lease pipe, serve argv.
                server = subprocess.Popen([sys.executable, '-c', 'import sys,time; time.sleep(300)',
                                           'serve', '--stdio', '--port', '0'], stdin=subprocess.PIPE,
                                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                pid_file = root / 'private-server.tmp'
                pid_file.write_text(str(server.pid))
                pid_file.replace(root / 'private-server')
        elif scenario == 'prompt-error':
            # OpenCode 2.1.3 answers the prompt this way once its model API refuses (HTTP 402, or
            # 429 after its retries); the process stays up. Some text may already have streamed.
            update({'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'partial'}})
            emit({'jsonrpc': '2.0', 'id': request['id'], 'error': {
                'code': -32603, 'message': 'Internal error: Insufficient balance; top up the account',
                'data': {'service': 'session', 'errorName': 'APIError'}}})
        else:
            update({'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': 'before operation'}})
            update({'sessionUpdate': 'tool_call', 'toolCallId': '1', 'title': 'Fixture output', 'status': 'in_progress'})
            update({'sessionUpdate': 'tool_call_update', 'toolCallId': '1', 'status': 'completed',
                    'content': [{'type': 'content', 'content': {'type': 'text', 'text': 'native tool output'}}]})
            vendor('session/prompt_complete')
            finish(request, 'reply: ' + texts[-1])
    elif method == 'session/cancel' and pending and permission is not None:
        # ACP: the client answers the pending permission request `cancelled` after the cancel.
        permission_log({'event': 'cancel'})
        cancel_seen = True
    elif method == 'session/cancel' and pending:
        if (root / 'scenario').read_text().strip() == 'server-drain':
            # A terminal beyond the host's 5s budget cannot prove cancellation.
            time.sleep(5.5)
        finish(pending, 'cancelled', 'cancelled')
        pending = None
    elif method is None and permission == 'grok-permission' and request.get('id') == permission:
        outcome = request['result']['outcome']
        permission_log({'event': 'reply', 'outcome': outcome})
        if outcome == {'outcome': 'selected', 'optionId': 'allow-once'} and (root / 'scenario').read_text().strip() == 'grok-mcp':
            write_report(pending)
        elif outcome.get('optionId') in ['allow-once', 'always-allow']:
            finish(pending, 'allowed')
        else:
            # Grok Build ends its turn on any answer but an allow.
            finish(pending, 'declined', 'cancelled')
        pending = permission = None
    elif method is None and permission == 'native-permission' and request.get('id') == permission:
        (root / 'permission-reply.json').write_text(json.dumps(request))
        assert request['result']['outcome'] == {'outcome': 'cancelled'}, 'permission policy is never'
        finish(pending, 'permission declined', 'cancelled')
        pending = permission = None
    elif method is None and permission is not None and request.get('id') == permission:
        outcome = request['result']['outcome']
        permission_log({'event': 'reply', 'outcome': outcome})
        if pending is None:
            continue
        if cancel_seen:
            finish(pending, 'cancelled', 'cancelled')
        else:
            finish(pending, 'permission: ' + outcome.get('optionId', outcome['outcome']))
        pending = permission = None
    elif 'id' in request:
        emit({'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32601, 'message': 'unknown method'}})
