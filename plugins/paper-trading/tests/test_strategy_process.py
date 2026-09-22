"""Exercise the real stdio and human-terminal strategy entry points."""
import json
import os
import pty
import select
import subprocess
import sys
import time

import pytest

from paper_trading.config import AccountConfig
from paper_trading.strategy import Portfolio
from .conftest import ROOT
from .test_process import Host, account_values


def settings(rig):
    return {'research_root': str(rig.research), 'symbols': ['SOXX.US'],
            'max_order_usd': '5000', 'max_portfolio_usd': '10000', 'max_trade_risk_usd': '100'}


def test_stdio_account_only_startup_and_unapproved_proposal_reports(rig, tmp_path):
    host = Host(rig, tmp_path / 'fresh')
    try:
        status = host.tool('paper.status', {})['structuredContent']
        assert status['strategy']['phase'] == 'unconfigured'
        proposal = host.tool('paper.strategy', settings(rig))['structuredContent']
        assert proposal['strategy']['phase'] == 'awaiting_approval'
        assert host.tool('paper.ingest', {'week': '2026-09-21'})['isError']
        assert host.tool('paper.approve', {'revision': proposal['strategy']['proposal']['revision']})['isError']
        assert host.tool('paper.strategy', settings(rig), track=None)['isError']
        assert host.tool('paper.strategy', settings(rig) | {'approved': True})['isError']
        while len({p['kind'] for p in host.overlays}) < 13:
            host.receive()
        strategy = next(p for p in host.overlays if p['kind'] == 'paper.strategy')
        assert strategy['entity_id'] == 'track-owner'
        assert any(row['approved'] == 'awaiting_approval' for row in strategy['payload']['rows'])
        overview = next(p for p in host.overlays if p['kind'] == 'paper.overview')['payload']
        assert overview['metrics'][0]['value'] == '—'
        assert rig.calls() == []
    finally:
        host.close()


def operator_args(rig, root, config, revision):
    return [sys.executable, '-m', 'paper_trading.operator', '--config', str(config),
            '--data-dir', str(root), '--approve-strategy', revision]


@pytest.mark.parametrize('answer,approved', [('', False), ('yes', False), ('APPROVE', True)])
def test_operator_strategy_waits_for_exact_human_approval(rig, tmp_path, answer, approved):
    root = tmp_path / 'fresh'
    account = AccountConfig.parse(account_values(rig))
    portfolio = Portfolio(root, account, rig.broker)
    proposal = portfolio.call('track-owner', 'paper.strategy', settings(rig))['strategy']['proposal']
    config = tmp_path / 'account.json'
    config.write_text(json.dumps(account_values(rig)))
    master, slave = pty.openpty()
    process = subprocess.Popen(operator_args(rig, root, config, proposal['revision']), cwd=ROOT,
                               stdin=slave, stdout=slave, stderr=slave)
    os.close(slave)
    output = b''
    try:
        deadline = time.monotonic() + 15
        while b'leave blank to stop:' not in output and time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:
                output += os.read(master, 65536)
        assert b'Type APPROVE' in output
        for expected in [b'PAPER123', b'track-owner', b'5000', b'10000', b'100', b'SOXX.US', proposal['revision'].encode()]:
            assert expected in output
        assert portfolio.call('track-owner', 'paper.status', {})['strategy']['active'] is None
        os.write(master, (answer + '\n').encode())
        assert process.wait(timeout=10) == 0
        status = portfolio.call('track-owner', 'paper.status', {})['strategy']
        assert (status['active'] is not None) is approved
        assert not any('--execute' in call for call in rig.calls())
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=3)
        os.close(master)


def test_operator_strategy_refuses_piped_confirmation(rig, tmp_path):
    root = tmp_path / 'fresh'
    account = AccountConfig.parse(account_values(rig))
    portfolio = Portfolio(root, account, rig.broker)
    revision = portfolio.call('track-owner', 'paper.strategy', settings(rig))['strategy']['proposal']['revision']
    config = tmp_path / 'account.json'
    config.write_text(json.dumps(account_values(rig)))
    result = subprocess.run(operator_args(rig, root, config, revision), cwd=ROOT,
                            input='APPROVE\n', capture_output=True, text=True, timeout=10)
    assert result.returncode == 2
    assert 'interactive terminal required' in result.stderr
    assert portfolio.call('track-owner', 'paper.status', {})['strategy']['active'] is None
    assert rig.calls() == []
