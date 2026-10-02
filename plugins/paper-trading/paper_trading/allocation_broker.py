"""Bounded SDK subprocess with the same explicit environment as supervised CLI reads."""
from pathlib import Path

from .broker import Broker, BrokerError, _object


class OrderNotSubmitted(BrokerError):
    """The trusted SDK bridge proved the broker write was never invoked."""


class AllocationBroker(Broker):
    def __init__(self, config, workdir):
        super().__init__(config.sdk_python_path, config.broker_home, workdir, timeout_seconds=30)
        self.config = config

    def request(self, method, body):
        # Typed configuration and exact operations only; no model-selected paths or endpoints.
        import json
        args = ['-I', str(Path(__file__).with_name('sdk_bridge.py')),
                '--access-region', self.config.access_region,
                '--client-id', self.config.oauth_client_id, '--account', self.config.account_no,
                '--cash-buffer-bps', str(self.config.cash_buffer_bps),
                '--max-order-bps', str(self.config.max_order_bps),
                '--quote-max-age-seconds', str(self.config.quote_max_age_seconds),
                method, '--request', json.dumps(body, allow_nan=False)]
        return _object(self._json(args))

    def snapshot(self, since):
        return self.request('snapshot', {'since': since})

    def submit(self, request):
        result = self.request('submit', request)
        if result == {'status': 'not_submitted'}:
            raise OrderNotSubmitted('SDK preflight refused before sending an order')
        if not isinstance(result.get('order_id'), str) or not result['order_id']:
            raise BrokerError('SDK returned no broker order identity; outcome may be unknown')
        return result['order_id']
