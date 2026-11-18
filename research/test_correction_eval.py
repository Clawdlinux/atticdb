import asyncio
import os

import pytest

from research import correction_eval


@pytest.mark.skipif(not (os.getenv("ATTICDB_BIN") and os.getenv("MCP_MEMORY_ENTRY")),
                    reason="set ATTICDB_BIN and MCP_MEMORY_ENTRY to run both servers")
def test_native_corrections_after_handoff():
    rows = asyncio.run(correction_eval.compare(os.environ["ATTICDB_BIN"], os.environ["MCP_MEMORY_ENTRY"]))
    assert len(rows) == 3
    for row, (_, _, _, first, stale) in zip(rows, correction_eval.CASES, strict=True):
        assert row["atticdb"] == {"current_only": True, "ordered_history": True,
                                  "stale_write_rejected": True, "values_after_overlap": [first]}
        assert row["reference"] == {"current_only": True, "ordered_history": None,
                                    "stale_write_rejected": False,
                                    "values_after_overlap": sorted([first, stale])}
