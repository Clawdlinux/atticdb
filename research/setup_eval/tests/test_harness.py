from research.setup_eval.harness import agent_finished
from research.setup_eval.parse import Outcome
from research.setup_eval.proc import Result


def test_a_pass_needs_a_clean_finish():
    ok, final = Result(0, b"", b"", 1.0), Outcome(reply="x", final=True)
    assert agent_finished(ok, final)
    assert agent_finished(ok, Outcome(reply="x", final=True, unparsed=3)), "garbage lines cost parse_ok only"
    assert not agent_finished(Result(1, b"", b"", 1.0), final), "a failing exit"
    assert not agent_finished(Result(None, b"", b"", 300.0, timed_out=True), final), "a timeout"
    assert not agent_finished(Result(None, b"", b"", 0.0, missing=True), final), "no agent"
    assert not agent_finished(ok, Outcome(reply="partial")), "no final reply"
    assert not agent_finished(ok, Outcome(reply="x", final=True, is_error=True)), "an error it reported"
