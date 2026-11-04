"""The 6 handoff cases. Harness A gets the writer prompts, harness B the reader prompt, in the same repo.

Writer prompts are plain user messages. They never mention memory, AtticDB or HANDOFF.md.
`evidence` holds tokens that prove a fact moved from A to B. They appear in the writer prompts and
the gold note, and never in a reader prompt or the fixture.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Case:
    id: str
    kind: str  # "answer": graded on B's reply. "code": graded on the repo and diff B leaves.
    writer_prompts: tuple[str, ...]
    reader_prompt: str
    gold_note: str
    evidence: tuple[str, ...]


H1_WRITER = ("Switch `pack_header` in ledger.py from JSON to `struct`. We measured JSON headers at 412 bytes "
             "vs 24 bytes with struct, see ticket LEDG-7731.")
H1_GOLD = ("pack_header in ledger.py uses struct instead of JSON. JSON headers measured 412 bytes vs 24 bytes "
           "with struct, see ticket LEDG-7731.")
H1_EVIDENCE = ("LEDG-7731", "412")

H2_WRITER = ("Keep `export` in ledger.py sequential. Never use threads or a pool there: ThreadPoolExecutor "
             "deadlocked on the shared SQLite handle in incident INC-2209. Add a docstring to export, nothing else.")
H2_GOLD = ("export in ledger.py stays sequential, with no threads or pool. ThreadPoolExecutor deadlocked on "
           "the shared SQLite handle in incident INC-2209.")

H3_CHANGE = ("We removed the SQLite handle from ledger.py today (change STOR-5150). Storage is plain files now, "
             "so the INC-2209 deadlock can't happen anymore.")
H3_GOLD = ("export in ledger.py was kept sequential because a thread pool deadlocked on the shared SQLite handle "
           "in INC-2209. Change STOR-5150 removed that handle and storage is plain files, so a thread pool is OK.")

H4_WRITER = ("Heads up: our CI runner `bramble-ci` only has Python 3.9. So no match statements anywhere in "
             "this repo.")
H4_GOLD = "The CI runner bramble-ci only has Python 3.9, so this repo uses no match statements anywhere."

H6_WRITER = ("Rule for ledger.py: money is always integer cents, never float. Bug FIN-88 lost a cent per 10k "
             "rows from float rounding.")
H6_GOLD = ("In ledger.py money is always integer cents, never float. Bug FIN-88 lost a cent per 10k rows "
           "from float rounding.")

CASES: dict[str, Case] = {c.id: c for c in (
    # H1 reason: B must give A's reason.
    Case("H1", "answer", (H1_WRITER,),
         "Why does ledger.py pack headers with struct instead of JSON? 1 or 2 lines.",
         H1_GOLD, H1_EVIDENCE),
    # H2 rejected: B must not redo the approach A ruled out.
    Case("H2", "code", (H2_WRITER,),
         "export() in ledger.py is too slow on big inputs. Make it faster.",
         H2_GOLD, ("INC-2209",)),
    # H3 changed: B must use the newer fact, not the older one.
    Case("H3", "answer", (H2_WRITER, H3_CHANGE),
         "Can export() in ledger.py use a thread pool now? Answer yes or no first, then why.",
         # INC-2209 is old news from prompt 1. Only the change's own tokens prove it was captured.
         H3_GOLD, ("STOR-5150", "plain files")),
    # H4 paraphrase: the question doesn't use the writer's words.
    Case("H4", "answer", (H4_WRITER,),
         "I want to use structural pattern matching in `parse` in ledger.py. Is that OK in this repo? "
         "Yes or no first.",
         H4_GOLD, ("bramble-ci", "3.9")),
    # H5 abstain: the store holds unrelated history only. B must say it has no record.
    Case("H5", "answer", (H1_WRITER,),
         "What retention period did we pick for exported ledger files? 1 line.",
         H1_GOLD, H1_EVIDENCE),
    # H6 constraint: B's new code must follow A's rule.
    Case("H6", "code", (H6_WRITER,),
         "Add `total(rows)` to ledger.py. It sums the `amount` field of each row dict.",
         H6_GOLD, ("FIN-88",)),
)}
