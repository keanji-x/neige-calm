"""Explicit account-level authorization for automatic SPY/cash paper execution."""
from dataclasses import dataclass
from pathlib import Path

from .config import exact, identifier


REQUIRED = frozenset(('profile', 'account_no', 'broker_home', 'owner_track_id',
                      'oauth_client_id', 'sdk_python_path'))
OPTIONAL = frozenset(('poll_seconds', 'cash_buffer_bps', 'drift_bps', 'quote_max_age_seconds', 'max_order_bps',
                      'access_region', 'opening_shares'))


@dataclass(frozen=True)
class AllocationConfig:
    account_no: str
    broker_home: str
    owner_track_id: str
    oauth_client_id: str
    sdk_python_path: str
    access_region: str = "global"
    max_order_bps: int = 1000
    poll_seconds: int = 60
    cash_buffer_bps: int = 200
    drift_bps: int = 100
    quote_max_age_seconds: int = 60
    opening_shares: int = 0

    @classmethod
    def parse(cls, values):
        exact(values, REQUIRED, OPTIONAL)
        if values['profile'] != 'spy_cash':
            raise ValueError('SPY allocation requires explicit spy_cash profile')
        obj = cls(**{k: v for k, v in values.items() if k != 'profile'})
        identifier(obj.account_no)
        if not isinstance(obj.broker_home, str) or '\0' in obj.broker_home or not Path(obj.broker_home).is_absolute():
            raise ValueError('broker_home must be an absolute path')
        if type(obj.poll_seconds) is not int or not 5 <= obj.poll_seconds <= 3600:
            raise ValueError('invalid poll_seconds')
        if not isinstance(obj.owner_track_id, str) or not obj.owner_track_id.strip() or len(obj.owner_track_id) > 256:
            raise ValueError('owner_track_id is required')
        identifier(obj.oauth_client_id)
        if obj.access_region not in ("global", "cn"):
            raise ValueError("access_region must be global or cn")
        if not isinstance(obj.sdk_python_path, str) or '\0' in obj.sdk_python_path or not Path(obj.sdk_python_path).is_absolute():
            raise ValueError('sdk_python_path must be absolute')
        for field, low, high in (('max_order_bps', 1, 10000), ('cash_buffer_bps', 1, 2000), ('drift_bps', 0, 1000),
                                 ('quote_max_age_seconds', 1, 120)):
            value = getattr(obj, field)
            if type(value) is not int or not low <= value <= high:
                raise ValueError(f'invalid {field}')
        if type(obj.opening_shares) is not int or obj.opening_shares < 0:
            raise ValueError('opening_shares must be a non-negative integer')
        return obj
