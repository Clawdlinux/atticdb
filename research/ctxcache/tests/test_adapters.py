import pytest

from research.ctxcache import copilot, openhands, swe_agent
from research.ctxcache.trace import check, tokens

from .helpers import (copilot_call, copilot_session, oh_bash, oh_cat, oh_row, shape, swe_obs, swe_row,
                      swe_window)

APP = "/repo/src/app.py"
TEST = "/repo/tests/test_app.py"
SEARCH = 'Found 1 matches for "value_7" in /repo:\n/repo/src/app.py (1 matches)\nEnd of matches for "value_7" in /repo'
FAILED_EDIT = ("Your proposed edit has introduced new syntax error(s). Please read this error message carefully.\n"
               "This is how your edit would have looked if applied\n" + swe_window(APP, 200, 1, 100, tag="?") +
               "\nYour changes have NOT been applied. Please fix your edit command and try again.")

SWE_CASES = [
    (
        "versions after edits, uses, re-reads and a URL",
        swe_row(
            ("open src/app.py", swe_obs(swe_window(APP, 200, 1, 100), APP)),
            ("scroll_down", swe_obs(swe_window(APP, 200, 99, 198), APP)),
            ("edit 150:150\n    value_150 = 0\nend_of_edit",
             swe_obs(swe_window(APP, 200, 99, 198, tag="#e", edited=True), APP)),
            ("open src/app.py", swe_obs(swe_window(APP, 200, 1, 100), APP)),
            ("python src/app.py", swe_obs("Traceback: boom", APP)),
            ("python src/app.py", swe_obs("Traceback: boom", APP)),
            ('search_dir "value_7"', swe_obs(SEARCH, APP)),
            ('search_dir "value_7"', swe_obs(SEARCH, APP)),
            ("edit 5:5\nbad(\nend_of_edit", swe_obs(FAILED_EDIT, APP)),
            ("create tests/test_app.py", swe_obs(f"[File: {TEST} (1 lines total)]\n1:", TEST)),
            ("rm tests/test_app.py", swe_obs("Your command ran successfully and did not produce any output.", TEST)),
            ("python src/app.py", swe_obs("Traceback: boom", TEST)),
            ("curl -s https://example.com/doc", swe_obs("<html>doc</html>", TEST)),
            ("open src/app.py", swe_obs(swe_window(APP, 200, 1, 100), APP)),
            ("edit 10:12\n    pass\nend_of_edit", None),
        ),
        # A, B: 2 windows of version 0. The edit uses B, the window that shows line 150.
        # D shows the same lines as A, but of version 1, so it is a new object. The run and the
        # failed edit use D. A rerun with the same text is a re-read (E, F) unless a write came
        # between (J).
        ["A*:file", "B*:file", "Br:file", "C*:file", "D*:file", "Dr:file", "E*:output", "Dr:file", "E:output",
         "F*:output", "F:output", "Dr:file", "G*:output", "H*:file", "I*:output", "Dr:file", "J*:output",
         "K*:url", "D:file", "Dr:file"],
        ["line_span", "run", "run", "line_span", "run", "line_span"],
        {"writes": 3, "actions_without_output": 1, "cluster": "owner__repo-1"},
    ),
    (
        "an edit goes to the open file, and a shell read is its own object",
        swe_row(
            ("open a.py", swe_obs(swe_window("/repo/a.py", 3, 1, 3), "/repo/a.py")),
            ("open b.py", swe_obs(swe_window("/repo/b.py", 3, 1, 3), "/repo/b.py")),
            ("edit 1:1\nx = 1\nend_of_edit", swe_obs(swe_window("/repo/b.py", 3, 1, 3, "#e", True), "/repo/b.py")),
            ("open a.py", swe_obs(swe_window("/repo/a.py", 3, 1, 3), "/repo/a.py")),
            ("cat b.py", swe_obs("x = 1\nraise KeyError('not found')", "/repo/a.py")),
            ("cat b.py", swe_obs("x = 1\nraise KeyError('not found')", "/repo/a.py")),
            ("cat c.py", swe_obs("cat: c.py: No such file or directory", "/repo/a.py")),
            ("edit 2:9\nx = 2\nend_of_edit", swe_obs(FAILED_EDIT, "/repo/a.py")),
        ),
        # A file that says "not found" is still read. A failed cat is an output.
        # The last edit runs past the 3 lines its window shows, so it is not an exact span.
        ["A*:file", "B*:file", "Br:file", "C*:file", "A:file", "D*:file", "D:file", "E*:output", "Ar:file",
         "F*:output"],
        ["line_span", "line"],
        {"writes": 1},
    ),
    (
        "a sed -i that prints its own error changed nothing, so the next edit still uses the window",
        swe_row(
            ("open src/app.py", swe_obs(swe_window(APP, 200, 1, 100), APP)),
            ("sed -i 's/value_1/v1' src/app.py", swe_obs("sed: -e expression #1, char 13: unterminated `s' command", APP)),
            ("edit 1:1\n    value_1 = 0\nend_of_edit", swe_obs(swe_window(APP, 200, 1, 100, tag="#e", edited=True), APP)),
        ),
        ["A*:file", "Ar:file", "B*:output", "Ar:file", "C*:file"],
        ["edit", "line_span"],
        {"writes": 1},
    ),
]


@pytest.mark.parametrize("name, row, expected, subs, info", SWE_CASES, ids=[c[0] for c in SWE_CASES])
def test_swe_agent_trace(name, row, expected, subs, info):
    acc, got = swe_agent.trajectory(row, "swe-agent:0")
    check(acc)
    assert shape(acc) == expected, name
    assert [a.sub for a in acc if a.ref] == subs, name
    assert {k: got.get(k) for k in info} == info, name
    window = row["trajectory"][3]["text"].split("\n\n(Open file:")[0]
    assert acc[0].tokens == tokens(window) < tokens(row["trajectory"][3]["text"]), "size and key share 1 body"


CAT_N_V1 = "\n".join(f"{i:6}\t{t}" for i, t in enumerate(["line ONE", "line two", "line three"], 1))
OH_CASES = [
    (
        "editor views, an edit that uses the view holding all of old_str, a created file, a shell read",
        oh_row(
            ("str_replace_editor", {"command": "view", "path": APP, "view_range": [1, 3]},
             oh_cat(APP, ["@property", "def size(self):", "    return 1"])),
            ("str_replace_editor", {"command": "view", "path": APP, "view_range": [10, 12]},
             oh_cat(APP, ["@property", "def name(self):", "    return 'x'"], first=10)),
            ("str_replace_editor", {"command": "str_replace", "path": APP, "old_str": "@property\ndef size(self):",
                                    "new_str": "@property\ndef size(self) -> int:"},
             oh_cat(APP, ["@property", "def size(self) -> int:"], edited=True)),
            ("str_replace_editor", {"command": "create", "path": "/repo/reproduce.py", "file_text": "import app\n"},
             "File created successfully at: /repo/reproduce.py"),
            ("execute_bash", {"command": "cd /repo && python reproduce.py"}, oh_bash("Traceback: boom", code=1)),
            ("execute_bash", {"command": "cd /repo && cat -n src/app.py"}, oh_bash(CAT_N_V1)),
            ("str_replace_editor", {"command": "view", "path": APP}, oh_cat(APP, ["line ONE", "line two", "line three"])),
            ("str_replace_editor", {"command": "insert", "path": APP, "insert_line": 2, "new_str": "line 2.5"},
             oh_cat(APP, ["line two", "line 2.5"], first=2, edited=True)),
            ("execute_bash", {"command": "cd /repo && git diff src/app.py"}, oh_bash("+line 2.5")),
            ("execute_bash", {"command": "grep -rn 'two' /repo/src"}, oh_bash("/repo/src/app.py:2:line two")),
            ("think", {"thought": "The fix works."}, "Your thought has been logged."),
            ("finish", {"message": "done"}, None),
        ),
        # The edit uses A, the view that holds all of old_str. B is newer and has its first line.
        # The run of the created file uses nothing: its content was never shown.
        # F: `cat -n` and the editor show the same lines.
        # The insert uses F, which shows line 2. git diff then names version 2.
        ["A*:file", "B*:file", "Ar:file", "C*:file", "D*:output", "E*:output", "F*:file", "F:file", "Fr:file",
         "G*:file", "Gr:file", "H*:output", "I*:output", "J*:output"],
        ["edit_span", "line_span", "git"],
        {"condensed": 0, "refs_without_view": 1, "writes": 3, "actions_without_output": 1},
    ),
    (
        "a second user message flags condensed history",
        oh_row(("str_replace_editor", {"command": "view", "path": APP}, oh_cat(APP, ["x"])),
               extra_user="Summary of the earlier steps."),
        ["A*:file"],
        [],
        {"condensed": 1},
    ),
    (
        "the auto-continue prompt is not condensed history",
        oh_row(("str_replace_editor", {"command": "view", "path": APP}, oh_cat(APP, ["x"])),
               extra_user="Please continue working on the task on whatever approach you think is suitable."),
        ["A*:file"],
        [],
        {"condensed": 0},
    ),
    (
        "a run that hit the iteration cap with fewer assistant messages flags condensed history",
        oh_row(("str_replace_editor", {"command": "view", "path": APP}, oh_cat(APP, ["x"])),
               exit_status="RuntimeError: Agent reached maximum iteration. Current iteration: 100, max iteration: 100"),
        ["A*:file"],
        [],
        {"condensed": 1},
    ),
    (
        "a shell read of a plain file keeps every line, even one that looks numbered",
        oh_row(
            ("execute_bash", {"command": "cd /repo && cat data.tsv"}, oh_bash("id\tname\n1\talpha\ntotal: 1")),
            ("execute_bash", {"command": "cd /repo && python gen.py"}, oh_bash("done")),
            ("execute_bash", {"command": "cd /repo && cat data.tsv"}, oh_bash("id\tname\n1\talpha\ntotal: 2")),
        ),
        # A script changed the file, and no write was seen. The 2 reads differ outside the
        # line "1<TAB>alpha", so they are 2 objects, not a re-read.
        ["A*:file", "B*:output", "C*:file"],
        [],
        {"writes": 0},
    ),
    (
        "a failed simple sed -i changes nothing, so a later edit still uses the view",
        oh_row(
            ("str_replace_editor", {"command": "view", "path": APP}, oh_cat(APP, ["x = 1", "y = 2"])),
            ("execute_bash", {"command": "cd /repo && sed -i 's/x/X' src/app.py"},
             oh_bash("sed: -e expression #1, char 7: unterminated `s' command", code=1)),
            ("str_replace_editor", {"command": "str_replace", "path": APP, "old_str": "x = 1", "new_str": "X = 1"},
             oh_cat(APP, ["X = 1"], edited=True)),
        ),
        ["A*:file", "Ar:file", "B*:output", "Ar:file", "C*:file"],
        ["edit", "edit_span"],
        {"writes": 1},
    ),
    (
        "a failed compound command and an unknown outcome keep their writes",
        oh_row(
            ("str_replace_editor", {"command": "view", "path": APP}, oh_cat(APP, ["x = 1"])),
            # Either step could have failed, so sed may have run.
            ("execute_bash", {"command": "cd /repo && sed -i 's/x/X/' src/app.py && python src/app.py"},
             oh_bash("Traceback: boom", code=1)),
            ("str_replace_editor", {"command": "view", "path": APP}, oh_cat(APP, ["X = 1"])),
            # No exit code: the command may still be running.
            ("execute_bash", {"command": "cd /repo && sed -i 's/X/Z/' src/app.py"}, "[Command is still running]"),
            ("str_replace_editor", {"command": "str_replace", "path": APP, "old_str": "Z = 1", "new_str": "z = 1"},
             oh_cat(APP, ["z = 1"], edited=True)),
        ),
        # The edit after the second sed finds no view of the version that sed made.
        ["A*:file", "Ar:file", "B*:output", "C*:file", "Cr:file", "D*:output", "E*:file"],
        ["edit", "edit"],
        {"writes": 3, "refs_without_view": 1},
    ),
    (
        "exit 1 is a no-match only from grep or rg, not from a path that names grep",
        oh_row(
            ("execute_bash", {"command": "cat /repo/grep.py"},
             oh_bash("cat: /repo/grep.py: No such file or directory", code=1)),
            ("execute_bash", {"command": "grep -n missing /repo/src/app.py"}, oh_bash("", code=1)),
        ),
        ["A*:output", "B*:file"],
        [],
        {},
    ),
    (
        "an edit with no tool result still uses its view, and writes nothing",
        oh_row(
            ("str_replace_editor", {"command": "view", "path": APP}, oh_cat(APP, ["x = 1"])),
            ("str_replace_editor", {"command": "str_replace", "path": APP, "old_str": "x = 1", "new_str": "X = 1"},
             None),
        ),
        ["A*:file", "Ar:file"],
        ["edit_span"],
        {"actions_without_output": 1, "writes": 0},
    ),
]


@pytest.mark.parametrize("name, row, expected, subs, info", OH_CASES, ids=[c[0] for c in OH_CASES])
def test_openhands_trace(name, row, expected, subs, info):
    acc, got = openhands.trajectory(row, "openhands:0")
    check(acc)
    assert shape(acc) == expected, name
    assert [a.sub for a in acc if a.ref] == subs, name
    assert {k: got.get(k, 0) for k in info} == info, name


A_PY = "/r/a.py"
COPILOT_CASES = [
    (
        "main-agent accesses, a sub-agent edit, a compaction, and a fork that is skipped",
        copilot_call("c1", "view", {"path": A_PY}, "print(1)\n")
        + copilot_call("c2", "edit", {"path": A_PY, "old_str": "print(1)", "new_str": "print(2)"}, "File updated.")
        + copilot_call("c3", "view", {"path": A_PY}, "print(2)\n")
        + copilot_call("s1", "view", {"path": A_PY}, "print(2)\n", parent="t1")
        + copilot_call("s2", "edit", {"path": A_PY, "old_str": "print(2)", "new_str": "print(3)"}, "ok", "t1", [A_PY])
        + copilot_call("c4", "bash", {"command": "cat /r/a.py"}, "print(3)\n")
        + [{"type": "session.compaction_complete", "data": {"success": True}}]
        + copilot_call("c5", "web_fetch", {"url": "https://x.dev/doc", "max_length": 100}, "doc text")
        + copilot_call("c6", "web_fetch", {"url": "https://x.dev/doc", "max_length": 100}, "doc text")
        + copilot_call("c7", "bash", {"command": "cat /r/a.py"}, "print(3)\n"),
        # The sub-agent's view is not an access, but its edit makes version 2, so D is new.
        ["A*:file", "Ar:file", "B*:output", "C*:file", "D*:file", "E*:url", "E:url", "D:file"],
        ["edit_span"],
        {"compactions": 1, "reread_same_window": 1, "reread_after_compaction": 1},
    ),
    (
        "a failed sed and an edit that changes nothing keep the version, and the shell trailer is chrome",
        copilot_call("c1", "view", {"path": A_PY}, "print(1)\n")
        + copilot_call("c2", "bash", {"command": "sed -i 's/1/2' /r/a.py"},
                       "sed: -e expression #1, char 5: unterminated `s' command\n<shellId: 7 completed with exit code 1>")
        + copilot_call("c3", "edit", {"path": A_PY, "old_str": "print(1)", "new_str": "print(2)"},
                       "File /r/a.py updated with changes.")
        + copilot_call("c4", "bash", {"command": "cat /r/a.py"}, "print(2)\n<shellId: 8 completed with exit code 0>")
        + copilot_call("c5", "edit", {"path": A_PY, "old_str": "print(2)", "new_str": "print(2)"},
                       "No changes made: old_str and new_str are the same")
        + copilot_call("c6", "bash", {"command": "cat /r/a.py"}, "print(2)\n<shellId: 9 completed with exit code 0>"),
        # The tool reports success on every command, so the exit code in the trailer decides.
        # Without the trailer's shell id, the 2 reads of version 1 are 1 object (D).
        ["A*:file", "Ar:file", "B*:output", "Ar:file", "C*:output", "D*:file", "Dr:file", "E*:output", "D:file"],
        ["edit", "edit_span", "edit_span"],
        {"writes": 1, "reread_same_window": 1},
    ),
]


@pytest.mark.parametrize("name, events, expected, subs, info", COPILOT_CASES, ids=[c[0] for c in COPILOT_CASES])
def test_copilot_trace(tmp_path, name, events, expected, subs, info):
    copilot_session(str(tmp_path), "main-session", events)
    copilot_session(str(tmp_path), "forked-session", events[:4])  # shares c1 and c2
    out = list(copilot.traces(str(tmp_path), salt="fixed"))
    assert [acc is None for _, _, acc, _ in out] == [False, True], "the fork is skipped"
    traj, _, acc, got = out[0]
    check(acc)
    assert shape(acc) == expected, name
    assert [a.sub for a in acc if a.ref] == subs, name
    assert {k: got.get(k, 0) for k in info} == info, name
    # Keys and ids are hashes: no path, URL or session name leaves a run.
    for text in [traj, *(a.obj for a in acc)]:
        assert len(text) in (12, 16) and all(c in "0123456789abcdef" for c in text), text
