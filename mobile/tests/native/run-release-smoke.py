#!/usr/bin/env python3
"""Drive the actual minified release APK from outside its process using adb."""
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET

MOBILE = Path(__file__).resolve().parents[2]
ANDROID = MOBILE / 'src-tauri/gen/android'
ARTIFACTS = MOBILE / 'artifacts/release-smoke'
SDK = Path(os.environ['ANDROID_HOME'])
BUILD_TOOLS = SDK / 'build-tools/35.0.0'
ANALYZER = SDK / 'cmdline-tools/latest/bin/apkanalyzer'
IP = 'http://10.0.2.2:5413'


def run(*args, timeout=20):
    return subprocess.check_output([str(arg) for arg in args], stderr=subprocess.STDOUT,
                                   text=True, timeout=timeout).strip()


def adb(*args):
    return run('adb', *args)


def hierarchy():
    adb('shell', 'uiautomator', 'dump', '/sdcard/neige-release-window.xml')
    return ET.fromstring(adb('shell', 'cat', '/sdcard/neige-release-window.xml'))


def structural_hierarchy(root):
    # Browser text, hints and accessibility descriptions may contain credentials.
    # Only these fixed structural attributes are safe to export.
    for node in root.iter():
        node.attrib = {key: node.attrib[key] for key in
                       ('package', 'class', 'bounds', 'enabled', 'clickable', 'focused', 'selected')
                       if key in node.attrib}
    return ET.tostring(root, encoding='unicode')


def wait_node(predicate, description, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            for node in hierarchy().iter('node'):
                if predicate(node.attrib):
                    return node.attrib
        except (subprocess.SubprocessError, ET.ParseError):
            pass  # Window transitions can temporarily prevent a UI dump.
        time.sleep(0.5)
    raise AssertionError(description)


def text_node(text, timeout=30):
    return wait_node(lambda n: n.get('text') == text and n.get('enabled') == 'true',
                     f'Missing enabled UI: {text}', timeout)


def tap(node):
    bounds = re.fullmatch(r'\[(\d+),(\d+)\]\[(\d+),(\d+)\]', node['bounds'])
    assert bounds, 'Missing visible node bounds'
    x1, y1, x2, y2 = map(int, bounds.groups())
    assert x2 > x1 and y2 > y1, 'Node is not visible'
    adb('shell', 'input', 'tap', (x1 + x2) // 2, (y1 + y2) // 2)


def launch(app_id):
    adb('shell', 'am', 'start', '-W', '-n', f'{app_id}/io.neigecalm.next.MainActivity')


def workspace():
    return wait_node(lambda n: '扫码连接你的工作区' in n.get('text', ''),
                     'IP did not load the actual bundled workspace login', 35)


def main():
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    apks = list((ANDROID / 'app/build/outputs/apk/universal/release').glob('*.apk'))
    assert len(apks) == 1, f'Expected one exact release APK, got {len(apks)}'
    source = apks[0]
    original_hash = hashlib.sha256(source.read_bytes()).hexdigest()
    assert run(ANALYZER, 'manifest', 'debuggable', source) == 'false', 'Release must not be debuggable'
    app_id = run(ANALYZER, 'manifest', 'application-id', source)
    assert app_id == 'io.neigecalm.next', 'Expected production release application ID'
    run('node', MOBILE / 'scripts/verify-bundled-apk.mjs', source)
    with tempfile.TemporaryDirectory(prefix='neige-release-smoke-') as temporary:
        temporary = Path(temporary)
        key = temporary / 'smoke.p12'
        apk = temporary / 'release.apk'
        shutil.copyfile(source, apk)
        run('keytool', '-genkeypair', '-keystore', key, '-storepass', 'smoke-only',
            '-keypass', 'smoke-only', '-alias', 'smoke', '-keyalg', 'RSA', '-keysize', '2048',
            '-validity', '2', '-dname', 'CN=Temporary release smoke', '-noprompt')
        run(BUILD_TOOLS / 'apksigner', 'sign', '--ks', key, '--ks-pass', 'pass:smoke-only', apk)
        run(BUILD_TOOLS / 'apksigner', 'verify', '--verbose', apk)
        run(BUILD_TOOLS / 'zipalign', '-c', '-P', '16', '4', apk)
        adb('install', '-r', apk)
        assert 'Success' in adb('shell', 'pm', 'clear', app_id)
        launch(app_id)
        tap(text_node('登录 Tailscale'))
        wait_node(lambda n: n.get('package') == 'com.android.chrome',
                  'Real release Tauri/JNI login did not open Chrome', 70)
        print('PASS: minified release login opens external Chrome', flush=True)
        launch(app_id)
        tap(text_node('Tailscale'))
        tap(text_node('IP 连接'))
        entry = wait_node(lambda n: n.get('class') == 'android.widget.EditText' and
                          n.get('enabled') == 'true', 'Missing IP input')
        tap(entry)
        adb('shell', 'input', 'text', IP)
        adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')  # Close keyboard.
        tap(text_node('保存并连接 IP'))
        workspace()
        print('PASS: configured IP loads APK-bundled workspace through release bridge', flush=True)
        adb('shell', 'am', 'force-stop', app_id)
        launch(app_id)
        workspace()
        print('PASS: IP configuration survives process death and automatically enters workspace', flush=True)
        tap(text_node('返回连接页'))
        text_node('保存并连接 IP')
        wait_node(lambda n: n.get('class') == 'android.widget.EditText' and n.get('text') == IP,
                  'Saved IP missing after returning to configuration')
        time.sleep(2)
        text_node('保存并连接 IP', 5)
        adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if not any(n.get('package') == app_id for n in hierarchy().iter('node')):
                break
            time.sleep(0.5)
        else:
            raise AssertionError('Back from configuration did not background the app')
        launch(app_id)
        text_node('保存并连接 IP')
        print('PASS: Back and reopening preserve accessible configuration', flush=True)
    assert hashlib.sha256(source.read_bytes()).hexdigest() == original_hash, 'Original release APK changed'
    (ARTIFACTS / 'result.txt').write_text(f'PASS: 4 release UI checks\nsourceApkSha256={original_hash}\n')


if __name__ == '__main__':
    try:
        main()
    except Exception:
        ARTIFACTS.mkdir(parents=True, exist_ok=True)
        try:
            xml = structural_hierarchy(hierarchy())
            (ARTIFACTS / 'failure-ui.xml').write_text(xml)
        except Exception:
            pass
        raise
