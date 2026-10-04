"""Native table helpers shared by every published SPY table."""
import pytest

from paper_trading.report_views import table


def test_table_does_not_default_missing_required_cell():
    with pytest.raises(KeyError):
        table([('required', 'Required')], [{}], 'Missing data')


@pytest.mark.parametrize('size', [2047, 2048, 2049])
def test_table_cell_unicode_boundary(size):
    text = '\U0001f642' * size
    displayed = table([('text', 'Text')], [{'text': text}], 'Boundary')['rows'][0]['text']
    assert len(displayed) <= 2048
    assert (displayed == text) is (size <= 2048)
