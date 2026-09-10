import importlib.util
from pathlib import Path
import unittest
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location('release_smoke', Path(__file__).with_name('run-release-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class ReleaseDiagnosticsTest(unittest.TestCase):
    def test_failure_artifact_omits_credentials_in_any_text_attribute(self):
        root = ET.Element('hierarchy')
        ET.SubElement(root, 'node', {
            'package': 'com.android.chrome', 'class': 'android.widget.EditText',
            'bounds': '[0,0][100,40]', 'enabled': 'true',
            'text': 'login.tailscale.com/a/secret-enrollment',
            'content-desc': 'https://login.tailscale.com/a/secret-enrollment',
            'hint': 'secret-enrollment', 'resource-id': 'secret-enrollment',
        })
        output = smoke.structural_hierarchy(root)
        self.assertNotIn('secret-enrollment', output)
        self.assertIn('com.android.chrome', output)
        self.assertIn('[0,0][100,40]', output)
