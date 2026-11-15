"""The 4 fixture projects. Each has a final tree whose code follows `code` style while its
CLAUDE.md rule asks for `rule` style. 2 git histories lead to that same tree:

- stale: the rule came first, then the code moved to the other style.
- migration: the code came first, then a last commit added the rule.
"""

from __future__ import annotations

import os
import subprocess
from dataclasses import dataclass

GENERIC = "# Project notes\n\n- Python 3.10+, standard library only.\n- Run tests with `python3 -m pytest -q`.\n"

# The paper's fix (arXiv 2610.30571), in the user-level file.
CODE_WINS = "If a project instruction file disagrees with the existing code, the existing code takes precedence.\n"
HISTORY = ("When a project rule and the existing code disagree, check git history first. A rule changed after "
           "the code it describes is the new direction: follow it. Code changed after the rule means the rule "
           "is stale: follow the code.\n")
ARMS = ("plain", "code-wins", "history", "annotated")
HISTORIES = ("stale", "migration")
CHANNELS = ("file", "memory")

# Pilot 2: the rule lives in a memory store, outside git, and reaches the agent through a
# SessionStart hook, as AtticDB's block does. `atticdb-today` is the block as it ships: the real
# header and dated lines, nothing else.
MEMORY_ARMS = ("plain", "code-wins", "history", "atticdb-today", "annotated")
MEMORY_HISTORY = ("When a remembered rule and the existing code disagree, find out which changed last. A rule "
                  "saved after the code it describes is the new direction: follow it. Code changed after the "
                  "rule was saved means the rule is stale: follow the code.\n")
ATTICDB_HEADER = ("AtticDB memory for repo project, a snapshot from chat start: rules, facts under repo/project/ "
                  "and only the 5 newest notes. Older notes and decisions are not shown. Before saying why or "
                  "when something was decided, call memory_search. Dates show when each line was saved. Before "
                  "acting on a fact about code, a version or a status, check it against the repo. If it is "
                  "wrong, set its key again. A line ending in (cut) was shortened. Call fact_get or "
                  "memory_search for the rest.")
PLAIN_HEADER = "AtticDB memory for repo project."
# When the memory was saved: with the stale history's 1st commit, or after the migration's last.
SAVED = {"stale": "2026-03-02", "migration": "2026-11-10"}
README = "# project\n\nA small example app.\n"

# Commit dates. The stale rule is old. The migration rule is the newest commit.
DATES = {"stale": ("2026-03-02T10:00:00", "2026-07-14T10:00:00", "2026-10-20T10:00:00"),
         "migration": ("2026-03-02T10:00:00", "2026-07-14T10:00:00", "2026-11-10T10:00:00")}


@dataclass(frozen=True)
class Convention:
    name: str
    rule: str          # the CLAUDE.md line
    module: str        # the file the task adds to
    task: str
    refactor: str      # the stale history's commit message moving to the code style
    helper: str        # the commit message adding the 2nd helper
    files: dict        # {"code": {path: text}, "rule": {path: text}}: the module and test in each style
    extra: dict        # the 2nd helper, in code style
    base: dict         # files in every commit


def _errors() -> Convention:
    code = {
        "app/books.py": ('BOOKS = {1: {"title": "Dune"}, 2: {"title": "Emma"}}\n\n\n'
                         'def get_book(book_id):\n    """The book with book_id, or None."""\n'
                         '    return BOOKS.get(book_id)\n'),
        "tests/test_books.py": ("from app.books import get_book\n\n\n"
                                "def test_get_book():\n    assert get_book(1)[\"title\"] == \"Dune\"\n"
                                "    assert get_book(9) is None\n"),
    }
    rule = {
        "app/books.py": ('BOOKS = {1: {"title": "Dune"}, 2: {"title": "Emma"}}\n\n\n'
                         'def get_book(book_id):\n    """The book with book_id. Raises LookupError."""\n'
                         '    try:\n        return BOOKS[book_id]\n    except KeyError:\n'
                         '        raise LookupError(f"no book {book_id}") from None\n'),
        "tests/test_books.py": ("import pytest\n\nfrom app.books import get_book\n\n\n"
                                "def test_get_book():\n    assert get_book(1)[\"title\"] == \"Dune\"\n"
                                "    with pytest.raises(LookupError):\n        get_book(9)\n"),
    }
    extra = {"app/loans.py": ("from app.books import get_book\n\n\n"
                              "def loan_title(book_id):\n    \"\"\"The title of a loaned book, or None.\"\"\"\n"
                              "    book = get_book(book_id)\n    if book is None:\n        return None\n"
                              "    return book[\"title\"]\n")}
    base = {"app/__init__.py": "", "tests/__init__.py": "",
            "app/members.py": 'MEMBERS = {7: {"name": "Ada"}, 8: {"name": "Lin"}}\n'}
    return Convention("errors", "- Lookups raise LookupError when the item is missing. They never return None.",
                      "app/members.py", "Add get_member(member_id) to app/members.py. It looks up a member in "
                      "MEMBERS by id. Add a test for it in tests/.",
                      "refactor: lookups return None for a missing item", "Add loan_title",
                      {"code": code, "rule": rule}, extra, base)


def _timestamps() -> Convention:
    code = {"app/events.py": ("import time\n\n\ndef new_event(name):\n"
                              "    return {\"name\": name, \"created\": int(time.time())}\n"),
            "tests/test_events.py": ("from app.events import new_event\n\n\ndef test_new_event():\n"
                                     "    assert isinstance(new_event(\"a\")[\"created\"], int)\n")}
    rule = {"app/events.py": ("from datetime import datetime, timezone\n\n\ndef new_event(name):\n"
                              "    return {\"name\": name, \"created\": datetime.now(timezone.utc).isoformat()}\n"),
            "tests/test_events.py": ("from app.events import new_event\n\n\ndef test_new_event():\n"
                                     "    assert new_event(\"a\")[\"created\"].endswith(\"+00:00\")\n")}
    extra = {"app/audit.py": ("import time\n\n\ndef audit_entry(action):\n"
                              "    return {\"action\": action, \"at\": int(time.time())}\n")}
    base = {"app/__init__.py": "", "tests/__init__.py": "", "app/comments.py": "MAX_LEN = 500\n"}
    return Convention("timestamps", "- Store times as ISO 8601 UTC strings, from "
                      "datetime.now(timezone.utc).isoformat(). Never as epoch numbers.",
                      "app/comments.py", "Add new_comment(text) to app/comments.py. It returns a comment record "
                      "with the text and when it was made. Add a test for it in tests/.",
                      "refactor: store times as epoch seconds", "Add audit_entry",
                      {"code": code, "rule": rule}, extra, base)


def _keys() -> Convention:
    test_head = "from types import SimpleNamespace\n\nfrom app.users import user_to_json\n\n\ndef test_user_to_json():\n"
    code = {"app/users.py": ("def user_to_json(user):\n"
                             "    return {\"user_id\": user.id, \"display_name\": user.name}\n"),
            "tests/test_users.py": test_head + ("    user = SimpleNamespace(id=1, name=\"Ada\")\n"
                                                "    assert user_to_json(user)[\"display_name\"] == \"Ada\"\n")}
    rule = {"app/users.py": ("def user_to_json(user):\n"
                             "    return {\"userId\": user.id, \"displayName\": user.name}\n"),
            "tests/test_users.py": test_head + ("    user = SimpleNamespace(id=1, name=\"Ada\")\n"
                                                "    assert user_to_json(user)[\"displayName\"] == \"Ada\"\n")}
    extra = {"app/items.py": ("def item_to_json(item):\n"
                              "    return {\"item_id\": item.id, \"unit_price\": item.price}\n")}
    base = {"app/__init__.py": "", "tests/__init__.py": "",
            "app/orders.py": ("from dataclasses import dataclass\n\n\n@dataclass\nclass Order:\n    id: int\n"
                              "    customer_name: str\n    total_cents: int\n")}
    return Convention("keys", "- API dicts use camelCase keys, such as userId. Never snake_case.",
                      "app/orders.py", "Add order_to_json(order) to app/orders.py. It returns a dict for the API "
                      "with the order's id, customer name and total. Add a test for it in tests/.",
                      "refactor: snake_case keys in API dicts", "Add item_to_json",
                      {"code": code, "rule": rule}, extra, base)


def _ids() -> Convention:
    code = {"app/notes.py": ("import itertools\n\n_ids = itertools.count(1)\n\n\ndef new_note(text):\n"
                             "    return {\"id\": next(_ids), \"text\": text}\n"),
            "tests/test_notes.py": ("from app.notes import new_note\n\n\ndef test_new_note():\n"
                                    "    assert isinstance(new_note(\"a\")[\"id\"], int)\n")}
    rule = {"app/notes.py": ("import uuid\n\n\ndef new_note(text):\n"
                             "    return {\"id\": uuid.uuid4().hex, \"text\": text}\n"),
            "tests/test_notes.py": ("from app.notes import new_note\n\n\ndef test_new_note():\n"
                                    "    assert len(new_note(\"a\")[\"id\"]) == 32\n")}
    extra = {"app/tags.py": ("import itertools\n\n_ids = itertools.count(1)\n\n\ndef new_tag(name):\n"
                             "    return {\"id\": next(_ids), \"name\": name}\n")}
    base = {"app/__init__.py": "", "tests/__init__.py": "", "app/tickets.py": "PRIORITIES = (\"low\", \"high\")\n"}
    return Convention("ids", "- Record ids are uuid.uuid4().hex strings. Never integers.",
                      "app/tickets.py", "Add new_ticket(title) to app/tickets.py. It returns a ticket record with "
                      "an id and the title. Add a test for it in tests/.",
                      "refactor: integer ids from a counter", "Add new_tag",
                      {"code": code, "rule": rule}, extra, base)


CONVENTIONS = {c.name: c for c in (_errors(), _timestamps(), _keys(), _ids())}


def git_env(date: str | None = None) -> dict:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env.update({"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", "GIT_AUTHOR_NAME": "dev",
                "GIT_AUTHOR_EMAIL": "dev@example.invalid", "GIT_COMMITTER_NAME": "dev",
                "GIT_COMMITTER_EMAIL": "dev@example.invalid", "GIT_TERMINAL_PROMPT": "0"})
    if date:
        env["GIT_AUTHOR_DATE"] = env["GIT_COMMITTER_DATE"] = date
    return env


def git(repo: str, *args: str, date: str | None = None) -> str:
    out = subprocess.run(["git", *args], cwd=repo, env=git_env(date), check=True, capture_output=True,
                         text=True, timeout=30)
    return out.stdout.strip()


def _write(repo: str, files: dict) -> None:
    for path, text in files.items():
        full = os.path.join(repo, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as f:
            f.write(text)


def _commit(repo: str, files: dict, message: str, date: str) -> None:
    _write(repo, files)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", message, date=date)


def _refactor(conv: Convention, neutral: bool) -> str:
    """The stale history's refactor message. Neutral names only the files, so dates are the only clue."""
    if not neutral:
        return conv.refactor
    return "Update " + " and ".join(sorted(conv.files["code"]))


def build(conv: Convention, history: str, repo: str, channel: str = "file", neutral: bool = False) -> str:
    """Builds the project at `repo`, which must not exist, with `history`. Returns HEAD's sha.
    With channel `memory`, the repo never holds the rule: its last migration commit adds a
    README instead, which the stale history has from the start."""
    if history not in HISTORIES or channel not in CHANNELS:
        raise ValueError((history, channel))
    os.makedirs(repo)
    git(repo, "init", "-q", "-b", "main")
    d = DATES[history]
    if channel == "memory":
        if history == "stale":
            _commit(repo, {**conv.base, **conv.files["rule"], "CLAUDE.md": GENERIC, "README.md": README},
                    "Initial version", d[0])
            _commit(repo, conv.files["code"], _refactor(conv, neutral), d[1])
            _commit(repo, conv.extra, conv.helper, d[2])
        else:
            _commit(repo, {**conv.base, **conv.files["code"], "CLAUDE.md": GENERIC}, "Initial version", d[0])
            _commit(repo, conv.extra, conv.helper, d[1])
            _commit(repo, {"README.md": README}, "Add README", d[2])
        return git(repo, "rev-parse", "HEAD")
    ruled = GENERIC + conv.rule + "\n"
    if history == "stale":
        _commit(repo, {**conv.base, **conv.files["rule"], "CLAUDE.md": ruled}, "Initial version", d[0])
        _commit(repo, conv.files["code"], _refactor(conv, neutral), d[1])
        _commit(repo, conv.extra, conv.helper, d[2])
    else:
        _commit(repo, {**conv.base, **conv.files["code"], "CLAUDE.md": GENERIC}, "Initial version", d[0])
        _commit(repo, conv.extra, conv.helper, d[1])
        _commit(repo, {"CLAUDE.md": ruled}, "Update CLAUDE.md", d[2])
    return git(repo, "rev-parse", "HEAD")


def provenance(conv: Convention, repo: str) -> str:
    """The line AtticDB could print next to the rule, from git alone."""
    sha = git(repo, "log", "-1", "--format=%h", "--", "CLAUDE.md")
    day = git(repo, "log", "-1", "--format=%cs", "--", "CLAUDE.md")
    since = int(git(repo, "rev-list", "--count", f"{sha}..HEAD", "--", "app", "tests"))
    return (f"Provenance of the project rule \"{conv.rule[2:]}\": CLAUDE.md last changed it on {day} "
            f"(commit {sha}). Since then, {since} commits changed app/ or tests/.\n")


def user_file(arm: str, conv: Convention, repo: str) -> str:
    """The user-level CLAUDE.md for `arm`."""
    if arm == "plain":
        return ""
    if arm == "code-wins":
        return CODE_WINS
    if arm == "history":
        return HISTORY
    if arm == "annotated":
        return HISTORY + "\n" + provenance(conv, repo)
    raise ValueError(arm)


def memory_block(arm: str, conv: Convention, history: str, repo: str) -> str:
    """The SessionStart block for pilot 2's `arm`."""
    rule = conv.rule[2:]
    key = f"repo/project/conventions/{conv.name}"
    if arm in ("plain", "code-wins", "history"):
        return f"{PLAIN_HEADER}\nThis repo (project):\n- {key}: {rule}"
    saved = SAVED[history]
    line = f"- {key} ({saved}): {rule}"
    if arm == "annotated":
        # The commit that was HEAD when the memory was saved, and the code commits since.
        anchor = git(repo, "rev-list", "-1", f"--before={saved}T23:59:59", "HEAD")
        since = int(git(repo, "rev-list", "--count", f"{anchor}..HEAD", "--", "app", "tests"))
        line += f" [saved at commit {anchor[:7]}. Since then, {since} commits changed app/ or tests/.]"
    if arm in ("atticdb-today", "annotated"):
        return f"{ATTICDB_HEADER}\nThis repo (project):\n{line}"
    raise ValueError(arm)


def memory_user_file(arm: str) -> str:
    """The user-level CLAUDE.md for pilot 2's `arm`."""
    return {"code-wins": CODE_WINS, "history": MEMORY_HISTORY, "annotated": MEMORY_HISTORY}.get(arm, "")
