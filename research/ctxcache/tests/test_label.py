import csv
import os
from collections import Counter

import pytest

from research.ctxcache import hf, label, run

from .helpers import cache_row, swe_obs, swe_row, swe_window

A = "/repo/a.py"


def test_label_sheet_is_stratified_and_lands_outside_the_repo(tmp_path):
    # Each row views a.py at step 0, then references that view twice: an edit by line
    # with no observation (line_span, step 1), then a run of the file (run, step 2).
    for off in hf.sample_offsets(hf.DATASETS["swe-agent"].rows, 2, 87):
        cache_row(str(tmp_path), "swe-agent", off, swe_row(
            ("open a.py", swe_obs(swe_window(A, 3, 1, 3), A)),
            ("edit 1:1\nx = 1\nend_of_edit", None),
            ("python a.py", swe_obs("ok", A))))
    sheet = tmp_path / "sheet.csv"
    repo_file = tmp_path / "repo-file.txt"  # a stand-in repo file, hard-linked at the sheet's path
    repo_file.write_text("keep")
    os.link(repo_file, sheet)
    argv = ["label-sample", "--dataset", "swe-agent", "--sample", "2", "--n", "3", "--cache-dir", str(tmp_path),
            "--offline", "--out", str(sheet)]
    assert run.main(argv) == 0
    assert repo_file.read_text() == "keep", "the sheet replaces the name, never writes through the link"

    rows = list(csv.DictReader(open(sheet)))
    assert list(rows[0]) == label.FIELDS
    # 3 split as evenly as the 2 subtypes allow.
    assert Counter(r["subtype"] for r in rows) == {"line_span": 2, "run": 1}
    assert all(r["needed"] == "" and r["note"] == "" for r in rows), "left for a human"
    assert all(r["view_excerpt"].startswith(f"[File: {A} (3 lines total)]") for r in rows)
    assert {r["action"].split("\n")[0] for r in rows} == {"edit 1:1", "python a.py"}
    # Accesses since the view count from the tool output, not from the reference before.
    assert {(r["subtype"], r["step"], r["accesses_since_view"]) for r in rows} == {("line_span", "1", "1"),
                                                                                   ("run", "2", "2")}

    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
    inside = os.path.join(repo, "sheet.csv")
    with pytest.raises(SystemExit) as e:
        run.main(argv[:-1] + [inside])
    assert e.value.code == 2 and not os.path.exists(inside), "a sheet never lands in the repo"
