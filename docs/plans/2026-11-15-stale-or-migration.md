# Stale Rule or Migration? Experiment Design

Status: pilot. Not a product change.

## Problem

A memory says "do X". The code does Y. The agent must pick.

There are 2 reasons the 2 can disagree:

- **Stale rule.** The code moved from X to Y after the rule was written. Follow the code.
- **Migration.** The team wrote the rule after the code. They want new code in X. Follow the rule.

The working tree is the same in both cases. Only history tells them apart.

## Prior art

- Xu and Wu, [arXiv 2610.30571](https://arxiv.org/abs/2610.30571), 22 Oct 2026. 40 Python projects, 320 runs. Without help, 90.1% of choices followed a stale `CLAUDE.md`. 1 sentence, "existing code takes precedence", moved 91.4% to the code. They suggest agent makers put that rule in the system prompt. They don't test a migration, where the same rule is wrong.
- GitHub Copilot Memory stores repo facts with code citations and checks them before use ([docs](https://docs.github.com/en/copilot/concepts/agents/copilot-memory)). It only works in Copilot, and its facts live in GitHub's cloud.
- `stalebrain` audits memory files against the repo on demand, using the model.
- STALE ([2605.06527](https://arxiv.org/abs/2605.06527)) and Supersede ([2606.27472](https://arxiv.org/abs/2606.27472)) measure stale memory in chat assistants, not code.
- Our users asked for this twice on X: "diff-linked rechecks", and "the date is when it was saved, not last checked".

## Hypotheses

- H1: With no help, agents follow the rule in both histories. So they are right on migrations and wrong on stale rules.
- H2: The code-wins sentence flips that. Right on stale rules, wrong on migrations.
- H3: Telling the agent to check history, or showing it the history, gets both right.

If H2 fails, the sentence is safe and we say so. That is still worth posting.

## Setup

- 4 conventions, each in its own tiny Python project with passing tests. Each direction is plausible both ways, so neither history reads as odd:
  - errors: raise `LookupError` or return `None` for a missing item
  - timestamps: ISO 8601 strings or Unix epoch integers
  - JSON keys: camelCase or snake_case
  - ids: `uuid4().hex` strings or incrementing integers
- 2 histories per project, with byte-identical final trees and the same number of commits:
  - stale: the rule first, then code commits that move to the other convention
  - migration: code first, then a last commit that adds the rule. Its message is neutral: `Update CLAUDE.md`
- The task asks for a feature that needs the convention, and never names it.
- 4 arms, all in the user-level `CLAUDE.md` of a fresh `CLAUDE_CONFIG_DIR`:
  - plain: nothing
  - code-wins: the paper's sentence
  - history: "When a project rule and the code disagree, check git history first. A rule changed after the code is the new direction. Code changed after the rule means the rule is stale."
  - annotated: the history sentence, plus 1 provenance line, as AtticDB could print it: when the rule was last changed, and how many commits changed the code since
- Harness: Claude Code, `claude -p`, `--model sonnet`, edits accepted, Bash only for `git`, `python3` and `pytest`. No MCP. Auto memory off. 600 s per run.

## Grading

Mechanical, on the lines the agent added. Each run is `rule`, `code`, `mixed` or `none`.
Correct is `code` for stale and `rule` for migration. We also record whether the agent ran `git log` or `git blame`, and whether tests pass.
Every row is read by hand before any number is reported.

## Pilot and stop rules

- Pilot: 4 conventions x 2 histories x 4 arms x 1 rep = 32 runs.
- If plain already gets both histories right, the problem isn't there. Stop and report.
- If annotated doesn't beat code-wins on migrations, there's no feature to build. Report the trade-off only.
- Only if the pilot shows the trade-off: 3 reps, and Copilot CLI as a 2nd harness. Codex is out of quota until 29 Nov.

## What this does not do

- No product change until the numbers are in and the owner approves.
- No claim beyond these 4 synthetic projects.
- No reuse of the paper's projects. Their data repo has no license.
