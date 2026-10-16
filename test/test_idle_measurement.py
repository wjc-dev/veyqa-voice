import pytest

from tools.measure_idle import cpu_seconds


@pytest.mark.parametrize("value,expected", [("0:00.23", 0.23), ("2:03.50", 123.5), ("1:02:03.00", 3723), ("2-01:00:00", 176400)])
def test_macos_process_cpu_time_parsing(value, expected):
    assert cpu_seconds(value) == expected
