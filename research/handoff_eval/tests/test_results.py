"""The CSV's new columns, the captured-with-reason table and `--regrade`. No agent and no atticdb
binary: these tests build rows by hand."""

from __future__ import annotations

import csv
import json
import os

import pytest

from research.handoff_eval import cli, results, run
from research.handoff_eval.run import HandoffResult

HERE = os.path.dirname(os.path.abspath(__file__))
PILOT = os.path.join(os.path.dirname(HERE), "results", "2026-11-04-pilot.csv")
# The CSV header before `captured_all` and `reply`, as the 2026-11-04 pilot has it.
OLD_COLUMNS = [c for c in results.COLUMNS if c not in ("captured_all", "reply")]


def res(case="H1", arm="atticdb", writer="claude-code", reader="copilot", **kw) -> HandoffResult:
    return HandoffResult(case, arm, writer, reader, 1, writer_model="X", reader_model="Y", **kw)


def write_csv(path, rows, columns=results.COLUMNS) -> None:
    with open(path, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=columns, extrasaction="ignore")
        w.writeheader()
        w.writerows(rows)


# ---- capture places ----

def test_store_hits_name_each_turn_by_lsn():
    """Each saved note is its own place, so LEDG-7731 in 1 note and 412 in another are 2 places."""
    line = json.dumps({"hits": [
        {"kind": "turn", "seq": 1, "lsn": 7, "content": "Switched to struct, LEDG-7731."},
        {"kind": "turn", "seq": 2, "lsn": 9, "content": "JSON headers were 412 bytes."},
        {"kind": "fact", "key": "repo/x/headers", "lsn": 11, "content": "struct: 24 vs 412 bytes (LEDG-7731)"}]})
    assert run.store_hits(line.encode(), "LEDG-7731") == ["atticdb:turn lsn 7", "atticdb:fact repo/x/headers"]
    assert run.store_hits(line.encode(), "412") == ["atticdb:turn lsn 9", "atticdb:fact repo/x/headers"]
    # A hit without an lsn keeps the old name.
    bare = json.dumps({"hits": [{"kind": "turn", "content": "LEDG-7731"}]}).encode()
    assert run.store_hits(bare, "LEDG-7731") == ["atticdb:turn"]


@pytest.mark.parametrize("found,want", [
    ([("atticdb:turn lsn 7", "LEDG-7731"), ("atticdb:turn lsn 9", "412")], False),
    ([("atticdb:turn lsn 7", "LEDG-7731"), ("atticdb:turn lsn 7", "412")], True),
    ([("repo:HANDOFF.md", "LEDG-7731"), ("repo:README.md", "LEDG-7731"), ("repo:README.md", "412")], True),
    ([("repo:HANDOFF.md", "LEDG-7731")], False),
    ([], False),
])
def test_all_in_one_place(found, want):
    assert run.all_in_one_place(found, ("LEDG-7731", "412")) is want


def test_parse_where_reads_where_back():
    found = [("atticdb:fact repo/evalrepo/x", "LEDG-7731"), ("atticdb:fact repo/evalrepo/x", "412"),
             ("repo:README.md", "LEDG-7731")]
    where = run._where(found)
    assert where == "atticdb:fact repo/evalrepo/x[LEDG-7731,412]; repo:README.md[LEDG-7731]"
    assert run.parse_where(where) == found
    assert run.parse_where("") == []
    with pytest.raises(ValueError):
        run.parse_where("repo:README.md")


# ---- the CSV ----

def test_reply_column_keeps_the_whole_reply(tmp_path):
    reply = "No.\n" + "x" * 500 + ", with a comma, \"quotes\" and a second line.\nEnd."
    r = res(case="H4", captured=True, captured_where="repo:HANDOFF.md[bramble-ci,3.9]", captured_all=True,
            passed=True, reply=reply, reasons=["reply=" + reply[:200]])
    path = tmp_path / "r.csv"
    results.append(str(path), [results.row("2026-11-04", r)])
    (back,) = results.read(str(path))
    assert back["reply"] == reply and back["captured_all"] == "1"


def test_append_refuses_a_file_with_other_columns(tmp_path):
    path = tmp_path / "old.csv"
    write_csv(path, [], OLD_COLUMNS)
    before = path.read_text()
    with pytest.raises(ValueError, match="has other columns"):
        results.append(str(path), [results.row("d", res())])
    assert path.read_text() == before


def test_summary_has_captured_with_reason():
    done = [res(captured=True, captured_all=False, passed=False),
            res(case="H2", captured=True, captured_all=True, passed=True),
            res(case="H3", errored=True, error_stage="reader", captured=True, captured_all=True),
            res(arm="native", captured=False, captured_all=False, passed=False),
            res(arm="oracle", passed=True)]
    text = "\n".join(results.summary(done))
    assert "## Captured with reason per arm" in text
    assert "| atticdb | 2/2 | 1/2 |" in text
    assert "| native | 0/1 | 0/1 |" in text
    assert "| oracle | n/a | n/a |" in text


# ---- --regrade ----

@pytest.mark.parametrize("reasons,want", [
    ("reader premium_requests=1; reply doesn't say it found no record; reply=I couldn’t find it; really.",
     "I couldn’t find it; really."),
    ("reply=Yes. STOR-5150.", "Yes. STOR-5150."),
    ("x; reply=No.; disconnect codex exited 1; codex refreshed its login; the eval dir's auth.json was updated",
     "No."),
    ("x; reply=" + "a" * 250, "a" * 200),
    ("error in reader: reader exited 1", None),
    ("", None),
])
def test_reply_from_reasons(reasons, want):
    assert results.reply_from_reasons(reasons) == want


def test_regrade_uses_the_reply_column_and_writes_nothing(tmp_path, capsys, monkeypatch):
    monkeypatch.setattr(run, "run_handoff", lambda *a, **k: pytest.fail("regrade started a run"))
    curly = "I couldn\u2019t find a recorded retention period for exported ledger files."
    rows = [
        # A stored fail that the current H5 grader passes.
        res(case="H5", captured=True, captured_where="atticdb:turn lsn 3[LEDG-7731,412]", passed=False,
            reply=curly, reasons=["reply doesn't say it found no record", "reply=" + curly]),
        # LEDG-7731 and 412 in 2 notes: captured, but not with the reason.
        res(captured=True, captured_where="atticdb:turn lsn 3[LEDG-7731]; atticdb:turn lsn 4[412]", passed=True,
            reply="LEDG-7731: 412 vs 24 bytes."),
        # A code case keeps its verdict, and an errored row stays errored.
        res(case="H2", captured=True, captured_where="repo:ledger.py[INC-2209]", passed=False, reply="Done."),
        res(case="H4", errored=True, error_stage="reader", captured=True,
            captured_where="repo:HANDOFF.md[bramble-ci,3.9]"),
        res(arm="oracle", case="H4", passed=True, reply="Yes, go ahead."),
    ]
    path = tmp_path / "r.csv"
    write_csv(path, [results.row("2026-11-04", r) for r in rows])
    before = (path.read_bytes(), sorted(os.listdir(tmp_path)))
    assert cli.main(["--regrade", str(path)]) == 0
    out = capsys.readouterr().out
    assert (path.read_bytes(), sorted(os.listdir(tmp_path))) == before
    assert "No agent ran and nothing was written." in out
    assert "3 answer rows were graded on the whole reply in the reply column." in out
    assert "may be cut off" not in out
    assert "- H5 atticdb claude-code to copilot rep 1: fail -> pass" in out
    assert "- H4 oracle claude-code to copilot rep 1: pass -> fail (reply starts with yes, not no;" in out
    assert "Code cases keep their stored verdict, as the repo they were graded on is gone: 1 rows." in out
    assert "Errored rows stay errored: 1." in out
    assert out.count("\n- ") == 2
    # Captured with reason, from captured_where: H5 in 1 note, H1 split over 2, H2's 1 token. H4 errored.
    assert "| atticdb | 3/3 | 2/3 |" in out
    assert "Integration pilot. Not a statistical result." in out


def test_regrade_of_a_csv_without_the_reply_column(tmp_path, capsys):
    long = "Yes. The thread ban was about a shared handle. " + "Details follow here. " * 10 + "STOR-5150 removed it."
    rows = [results.row("2026-11-04", r) for r in (
        res(case="H5", passed=False, reasons=["reply doesn't say it found no record",
                                              "reply=I couldn\u2019t confirm a retention period from the history.",
                                              "disconnect copilot exited 1"]),
        res(case="H3", passed=True, reasons=["reply=" + " ".join(long.split())[:200]]),
        res(case="H1", passed=False, reasons=["writer 1 premium_requests=1"]),
    )]
    path = tmp_path / "old.csv"
    write_csv(path, rows, OLD_COLUMNS)
    assert cli.main(["--regrade", str(path)]) == 0
    out = capsys.readouterr().out
    assert "This CSV has no reply column. 2 answer rows were graded on the reply= text in the reasons." in out
    assert "so those replies may be cut off. 1 of them are at the limit." in out
    assert "- H5 atticdb claude-code to copilot rep 1: fail -> pass" in out
    assert ("- H3 atticdb claude-code to copilot rep 1: pass -> fail (reply cites neither STOR-5150 nor plain-file "
            "storage) [reply cut at 200 chars: the flip may come from the missing text]") in out
    assert "1 of 2 changed verdicts were graded on a cut reply." in out
    assert "Kept: H1 atticdb claude-code to copilot rep 1: no reply= in the reasons, stored verdict kept" in out


@pytest.mark.parametrize("content,why", [(None, "No such file"), ("date,case\n2026-11-04,H1\n", "has no arm")])
def test_regrade_of_a_bad_csv_exits_2(tmp_path, capsys, content, why):
    path = tmp_path / "bad.csv"
    if content is not None:
        path.write_text(content)
    assert cli.main(["--regrade", str(path)]) == 2
    assert why in capsys.readouterr().err


def test_regrade_and_check_dont_mix(capsys):
    with pytest.raises(SystemExit) as e:
        cli.parse(["--check", "--regrade", "x.csv"])
    assert e.value.code == 2


def test_regrade_of_the_pilot_matches_the_hand_audit():
    """The 2026-11-04 pilot CSV has no reply column. The 4 H5 grader misses of the hand audit flip to pass.
    The 5th, H4 files, stays a fail: it cites HANDOFF.md's 3.9 but not bramble-ci, and the plan owner's
    ruling passes H4 only on bramble-ci. Every other flip comes from a reply cut at 200 chars."""
    with open(PILOT, newline="") as f:
        rows = list(csv.DictReader(f))
    rg = results.regrade(rows)
    fixed = {f"H5 {a} claude-code to copilot rep 1: fail -> pass" for a in ("native", "files", "atticdb", "oracle")}
    assert fixed <= set(rg.changed)
    assert not any(line.startswith("H4 files claude-code to copilot") for line in rg.changed)
    h4 = {(r.arm, r.writer): r.passed for r in rg.results if r.case == "H4"}
    assert h4[("files", "claude-code")] is False
    for line in set(rg.changed) - fixed:
        assert line.endswith("[reply cut at 200 chars: the flip may come from the missing text]"), line
    assert "H4 passes only when the reply cites bramble-ci. Hand-audit H4 fails." in results.regrade_report(PILOT, rg)
