"""Native Report presentation helpers: inert view, row, cell and table data."""
from datetime import datetime
import hashlib
import json

from .report_text import bounded


def native_view(state, title, rows, description=''):
    """Publish inert composition data with an identity derived from this projection.

    Reconciliation supplies the observation time. This pure projection has no
    publication clock, so producedAt stays explicitly unknown. Event times
    never stand in for when the projection was generated.
    """
    observed = state['snapshot']['at'] if state['snapshot'] else None
    observed = int(datetime.fromisoformat(observed.replace('Z', '+00:00')).timestamp() * 1000) if observed is not None else None
    identity = hashlib.sha256(json.dumps([title, description, rows, observed], ensure_ascii=False,
                                         sort_keys=True, allow_nan=False).encode()).hexdigest()
    return {'version': 1, 'title': title, 'description': description,
            'snapshot': {'id': identity, 'observedAt': observed, 'producedAt': None}, 'rows': rows}


def row(identity, cells, title, layout):
    return {'id': identity, 'title': title, 'layout': layout, 'cells': cells}


def scalar(amount, unit='$', decimals=0, signed=False, placement='prefix'):
    return {'state': 'known', 'amount': amount, 'unit': unit, 'decimals': decimals,
            'signed': signed, 'placement': placement}


def unknown(reason):
    return {'state': 'unknown', 'reason': reason}


def metric(key, label, value, detail, tone='neutral', primary=False):
    return {'id': key, 'label': label, 'value': value, 'detail': detail, 'tone': tone,
            'emphasis': 'primary' if primary else 'normal'}


def record(identity, title, summary, subtitle='', badges=None, facts=None, sections=None, disclosures=None):
    return {'id': identity, 'subtitle': subtitle, 'title': title, 'summary': summary,
            'badges': badges or [], 'facts': facts or [], 'sections': sections or [], 'disclosures': disclosures or []}


def records(identity, title, empty_text, items, label=None, description=''):
    return {'kind': 'records', 'id': identity, 'title': title, 'emptyText': empty_text,
            'datasets': [{'id': identity + '-items', 'label': title if label is None else label,
                          'description': description, 'items': items}]}


def display_cell(value):
    # Native table cells allow 2048 Unicode code points. Ledger and tool responses stay complete.
    return bounded(value) if isinstance(value, str) else value


def table(columns, rows, caption):
    return {"columns": [{"key": key, "label": label} for key, label in columns],
            "rows": [{key: display_cell(row[key]) for key, _label in columns} for row in rows], "caption": caption}
