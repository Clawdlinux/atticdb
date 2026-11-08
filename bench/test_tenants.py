"""Tests for the pure parts of bench/tenants.py. Run: python3 -m pytest bench/test_tenants.py -q"""

import os
import sys

sys.path.insert(0, os.path.dirname(__file__))

import tenants  # noqa: E402


def test_config_json_lists_every_tenant_and_the_segment_size():
    config = tenants.config_json([("t0", "a" * 32), ("t1", "b" * 32)], 16)
    assert config == {
        "tenants": [{"name": "t0", "token": "a" * 32}, {"name": "t1", "token": "b" * 32}],
        "segment_mib": 16,
    }


def test_pick_active_count_is_rounded_with_a_floor_of_1():
    assert len(tenants.pick_active(100, 5, seed=1)) == 5
    assert len(tenants.pick_active(100, 20, seed=1)) == 20
    assert len(tenants.pick_active(10, 1, seed=1)) == 1
    assert len(tenants.pick_active(1, 20, seed=1)) == 1
    assert len(tenants.pick_active(1000, 1, seed=1)) == 10


def test_pick_active_is_deterministic_per_seed_and_in_range():
    first = tenants.pick_active(100, 20, seed=7)
    assert first == tenants.pick_active(100, 20, seed=7)
    assert len(set(first)) == len(first)
    assert all(0 <= i < 100 for i in first)
    assert first != tenants.pick_active(100, 20, seed=8)


def test_percentile_uses_nearest_rank():
    samples = [float(v) for v in [7, 1, 10, 3, 5, 2, 9, 4, 8, 6]]
    assert tenants.percentile(samples, 50) == 5.0
    assert tenants.percentile(samples, 90) == 9.0
    assert tenants.percentile(samples, 99) == 10.0
    assert tenants.percentile(samples, 100) == 10.0
    assert tenants.percentile(samples, 0) == 1.0
    assert tenants.percentile([4.0], 99) == 4.0


def test_lsof_fd_count_skips_cwd_txt_and_mem_rows():
    text = "p12\nfcwd\nftxt\nftxt\nf0\nf1\nf2\nf44\np13\nfcwd\nf0\nf5\n"
    assert tenants.count_lsof_fds(text) == 6


def test_ps_thread_count_drops_the_header():
    text = (
        "USER PID TT %CPU STAT PRI STIME UTIME COMMAND\n"
        "me 12 ?? 0.0 S 31T 0:00.01 0:00.00 atticdb\n"
        "   12    0.0 S 31T 0:00.00 0:00.00\n"
        "me 13 ?? 0.0 S 31T 0:00.01 0:00.00 atticdb\n"
    )
    assert tenants.count_ps_threads(text) == 3
