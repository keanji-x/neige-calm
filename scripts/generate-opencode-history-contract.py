"""Used OpenAPI contract, projected from the official pinned SDK document.

Regenerate only for an explicitly reviewed adapter upgrade:
python3 -I scripts/generate-opencode-history-contract.py /path/to/official/packages/sdk/openapi.json
"""
import json
from pathlib import Path
import sys

class Refused(Exception):
    pass


def require(ok, category):
    if not ok:
        raise Refused(category)

VERSION = '1.19.3'
COMMIT = 'e3c4eb242585dcd77be1a867391b7c25fd47ce52'
ROUTES = ('/global/health', '/project', '/project/current', '/session', '/session/{sessionID}',
          '/session/{sessionID}/message', '/session/{sessionID}/message/{messageID}')
ANNOTATIONS = {'description', 'summary', 'title', 'examples', 'example', 'x-codeSamples'}


def clean(value):
    if isinstance(value, dict):
        return {k: ({name: clean(schema) for name, schema in v.items()}
                    if k == 'properties' else clean(v))
                for k, v in value.items() if k not in ANNOTATIONS}
    if isinstance(value, list):
        return [clean(v) for v in value]
    return value


def project(doc):
    """Ignore unrelated endpoints and prose; pin every used request/response schema fact."""
    try:
        paths, schemas = {}, {}
        for route in ROUTES:
            get = doc['paths'][route]['get']
            paths[route] = {'get': {'operationId': get['operationId'],
                                    'parameters': clean(get['parameters']),
                                    'responses': {'200': {'content': {'application/json': {
                                        'schema': clean(get['responses']['200']['content']['application/json']['schema'])}}}}}}

        def visit(value):
            if isinstance(value, dict):
                if '$ref' in value:
                    ref = value['$ref']
                    require(ref.startswith('#/components/schemas/'), 'contract: reference')
                    name = ref.split('/')[-1]
                    if name not in schemas:
                        schemas[name] = clean(doc['components']['schemas'][name])
                        visit(schemas[name])
                for v in value.values():
                    visit(v)
            elif isinstance(value, list):
                for v in value:
                    visit(v)
        visit(paths)
        return {'openapi': doc['openapi'], 'paths': paths, 'components': {'schemas': schemas}}
    except (KeyError, TypeError, RecursionError):
        raise Refused('contract: unsupported OpenAPI') from None


if __name__ == '__main__':
    raw = Path(sys.argv[1]).read_bytes()
    (Path(__file__).resolve().parents[1] / 'crates/plugin/src/builtin/opencode_history/contract.json').write_text(
        json.dumps(project(json.loads(raw)), ensure_ascii=True, sort_keys=True, indent=2) + '\n')
