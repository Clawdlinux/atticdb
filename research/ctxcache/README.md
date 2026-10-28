# ctxcache

How far is coding-agent context management from optimal?
This directory models the context window as a cache over a durable log.
The log is the agent's trajectory: anything that left the window can be read again, at the cost of its tokens.
We replay real trajectories through eviction policies.
Then we bracket the offline optimum between a lower and an upper bound.
Issue #87 has the motivation. First results are in [results/](results/README.md).

Python 3, standard library only. Tests use pytest.

## Run

```sh
python3 -m research.ctxcache --dataset swe-agent --sample 1000
python3 -m research.ctxcache --dataset openhands --sample 1000 --block 10
python3 -m research.ctxcache --dataset copilot
python3 -m research.ctxcache --dataset trace --input research/ctxcache/fixtures/tiny-trace.jsonl
python3 -m pytest research/ctxcache/tests
```

`python3 research/ctxcache/run.py` takes the same flags.

| Flag | Default | Meaning |
|---|---|---|
| `--dataset` | | `swe-agent`, `openhands`, `copilot` or `trace` |
| `--sample` | 1000 | trajectories to sample from a public set |
| `--block` | 1 | consecutive rows per stratum |
| `--seed` | 87 | the sample and bootstrap seed |
| `--budgets` | `1k,2k,4k,8k,16k,32k,64k,128k` | token budgets. 1k is 1,000 tokens |
| `--out` | `research/ctxcache/results` | where the CSV and markdown go |
| `--cache-dir` | `~/.cache/zerodb-ctxcache` | downloaded rows, outside the repo |
| `--sleep` | 1.6 | seconds between API calls |
| `--max-requests`, `--deadline` | 2500, 5400 | caps on API calls and on seconds spent fetching |
| `--offline` | off | use cached rows only |
| `--export-trace` | | also write the hashed trace. Refused inside the repo |
| `--bootstrap` | 1000 | cluster bootstrap resamples. 0 turns it off |
| `--drop-refs` | off | keep re-reads only, to test the reference proxy. Output names end in `-rereads` |
| `--input`, `--copilot-root` | | a trace file, or another Copilot log directory |

A run writes 3 files to `--out`: `<dataset>.csv` has 1 row per budget and policy, `<dataset>-trace.csv` has the trace measures, and `<dataset>.md` shows both.

## The model

The context window is a cache of `B` tokens. These rules define objects and accesses. `trace.py` applies them, and the adapters call it.

**Objects.** An object is a piece of content that a tool output put in context. There are 3 kinds.
- `file`: a file version as shown. The key is the path, the version and the exact lines shown. The version counts the writes to that path so far. 2 outputs that show the same lines of the same version are 1 object. Other lines of the same version, overlapping windows included, are other objects.
- `url`: a fetched URL. The key is the URL, the request and the text.
- `output`: any other tool output, such as a test run, a listing, a search over many files, or an error. The key is the tool, the request, the text, and the number of writes so far in the trajectory. A rerun that returns the same text with no write in between is the same object. After any write it is a new one.

**Accesses.** Each access is 1 trace line. There are 2 sources.
- Every tool output is 1 access to its object. The first access to an object is a compulsory miss. A later one is a re-read: a hit if the object is still held, else a miss that costs its tokens.
- A reference is a later action that names a path at its current version, or a URL, without reading it. Examples are an edit of the file, a run of the script, and `git diff` on it. It is 1 access to the newest view of that version. An action that names a line (`edit 150:152`, `insert_line`) uses the newest view that shows the line. An edit with old text (`old_str`) uses the newest view that holds all of it. Failing that, it uses the newest view with its longest line. str_replace needs that text verbatim in the file. With no view of the current version there is nothing to reference, so there is no access. We count those cases.

**Versions.** A write makes a new version. Writes are edits, creates, deletes, moves, `sed -i` and shell redirects into a file. Every object of the old version is dead after it. A write is not an access. An edit is a reference to the version it edits, then a write. When the edit's output shows the new lines, that output is the first access to the new version.

**Sizes.** Tokens are characters / 4, at least 1, of the whole tool output. An object's size is fixed at its first access. A missed reference costs that size.

**Outside the model.** Only the main agent counts. The system prompt, tool definitions, the task text and the agent's own messages are outside the budget. Results report message tokens next to tool output tokens.

## Trace format

1 JSONL line per access. These 2 lines open sampled row 6681: `create reproduce.py` shows a 1-line window, then `edit 1:1` references it.

```json
{"traj": "swe-agent:6681", "t": 0, "obj": "6ee1cca095f41078", "tokens": 34, "kind": "file", "first": true, "ref": false}
{"traj": "swe-agent:6681", "t": 1, "obj": "6ee1cca095f41078", "tokens": 34, "kind": "file", "first": false, "ref": true}
```

| Field | Meaning |
|---|---|
| `traj` | the trajectory: `<dataset>:<row offset>`, or a salted hash for our own logs |
| `t` | the access index in the trajectory, from 0 |
| `obj` | the first 16 hex digits of a sha256 over the object key, so no path or content leaves a run |
| `tokens` | the object's size |
| `kind` | `file`, `url` or `output` |
| `first` | the first access to the object: a compulsory miss |
| `ref` | true for a reference by a later action, false for a tool output |

`ref` is the 1 field beyond the format in issue #87. Tool-result clearing needs it, because a reference does not refresh a cleared output.
`trace.load` rejects a trace whose `first` flags do not match each object's history, or whose sizes change.
`--export-trace` writes this format and `--dataset trace` reads it. [fixtures/tiny-trace.jsonl](fixtures/tiny-trace.jsonl) is a synthetic example.

## Policies

Every policy runs under 1 cache model. The brute-force optimum uses the same one.
- After every access, the held objects total at most `B` tokens.
- An access hits if its object is held. Only the accessed object can enter. Any object can leave at any time, for free.
- A policy may skip admitting an object (bypass). An object larger than `B` is never held.

Each policy returns misses and missed tokens over non-first accesses. First accesses miss under every policy, so they are reported once, as the compulsory share.

| Policy | Kind | Rule |
|---|---|---|
| LRU | online | Evict the least recently accessed object. |
| FIFO | online | Evict the oldest insertion. A hit changes nothing. This keeps the newest tokens, which threshold compaction approximates. |
| Clearing | online | Hold the objects of the newest N tool outputs. References do not refresh. This is the shape of tool-result clearing in Anthropic's context editing. N is the largest value whose held tokens, averaged over the gaps between accesses, fit in `B`. N is picked per trajectory, which favors clearing, and it can exceed `B` at peaks. |
| GDSF | online | Greedy-Dual-Size-Frequency (Cherkasova 1998) with cost 1. Priority is L + frequency / size. Evict the lowest and raise L to it. |
| Belady-Size | offline, upper bound | On a miss, admit the object only if it fits after evicting held objects used later than it. Those go furthest next use first, larger first. It is feasible, so the optimum is at most this. |
| PFOO-L | offline, lower bound | Berger, Beckmann, Harchol-Balter, SIGMETRICS 2018, arXiv 1711.03709. The capacity must hold only on average over the n - 1 gaps between accesses. A hit needs its reuse interval held, at a cost of size x length. For misses, keep the cheapest intervals. For missed tokens, solve a fractional knapsack, shortest intervals first. Intervals of objects larger than `B` never count. Every feasible schedule fits this relaxation, so the optimum is at least this. |
| OPT | exact, tiny traces | `policies.opt` searches every hold and evict choice, for at most 16 accesses. It finds the fewest misses and the fewest missed tokens separately. |

`test_bounds.py` replays 500 random traces of at most 12 accesses, with seed 87. It asserts PFOO-L <= OPT <= Belady-Size for misses and for missed tokens. It also asserts that OPT is at most LRU, FIFO and GDSF, and that PFOO-L is at most Clearing.
An earlier draft took PFOO-L's missed tokens from the count-optimal intervals. That is not a bound: on random tiny traces it exceeded the true optimum in 5 of 5,000.

## Measures

- **Miss ratio**: misses over non-first accesses. **Token miss ratio**: missed tokens over the tokens of non-first accesses. Both pool all trajectories.
- **Compulsory share**: first accesses over all accesses, by count and by tokens.
- **Dead tokens**: tokens of objects never accessed after their first access, over the tokens of all objects. Reported overall, for files and for outputs. Most outputs are dead by construction, since only an unchanged rerun can access them again.
- **Reuse distance**: for each non-first access, the tokens of distinct other objects accessed since the previous access to the same object. This is the token-weighted LRU stack distance. When no object exceeds the budget, LRU hits exactly when it plus the object's size fits. p50, p90 and p99 pool all non-first accesses.
- **Peak live tokens**: per trajectory, the most tokens that are live at once. An object is live from an access until its next access. The optimum never misses at this budget.
- **Gap to the optimum**: a policy's missed tokens over Belady-Size's, then over PFOO-L's. The policy's multiple of the true optimum lies between the 2. `inf` means the lower bound is 0.
- **Intervals**: 95% percentile intervals from 1,000 bootstrap resamples of instances, or of sessions for our logs, with replacement.

## Adapters

### SWE-agent: `swe_agent.py`

A row of nebius/SWE-agent-trajectories has `trajectory`, a list of steps with `role` (`system`, `user` or `ai`) and `text`.
The first `user` step is the task.
Each `ai` step holds 1 action: its last top-level fenced block, the block SWE-agent itself runs.
The next `user` step is the observation. It ends with `(Open file: <path>)`, `(Current directory: <dir>)` and `bash-$`.
That footer gives the open file and the working directory for the next action.

| Action | Rule |
|---|---|
| `open`, `goto`, `scroll_down`, `scroll_up` | A file read. The object is the window: the `[File: <path> (N lines total)]` header, the numbered lines and the "more lines" markers. A failure such as "File x not found" is an output. |
| `create <path>` | A write, then a read of the new window. "Error: File 'x' already exists" followed by a window is a read only. |
| `edit <s>:<e>` | A reference to the open file's newest window that shows line s, else its newest window. If the output says "File updated.", a write, then a read of the new window. A failed edit (syntax error, "NOT been applied") is an output and leaves the version alone. |
| `search_file <term> [<file>]` | A read of that file: the list of matching lines. |
| `search_dir`, `find_file`, `submit` | Outputs. |
| anything else | A shell command, below. |
| an `ai` step with no observation | Its edit still references the open file. |

### OpenHands: `openhands.py`

A row of nebius/SWE-rebench-openhands-trajectories has `trajectory`, a list of messages.
They are `system`, `user` (the task), then `assistant` messages with 1 tool call each.
`tool` messages with the same `tool_call_id` answer them. Arguments are a JSON string.

| Tool call | Rule |
|---|---|
| `str_replace_editor view` of a file | A read. The object is the `cat -n` lines shown, from `view_range` or the whole file. |
| `view` of a directory, or an error | An output. |
| `create` | "File created successfully" is a write. The output only confirms it, so the new content is never shown. |
| `str_replace`, `insert` | A reference to a view of the current version, by `old_str` or `insert_line` as above, else the newest view. On "has been edited", a write, and the snippet shown is a read of the new version. |
| `undo_edit` | On success, a write, and the file shown is a read. |
| `execute_bash` | A shell command, below. `[Current working directory: <dir>]` in the output sets the directory for the next call. A pure read that exits nonzero is an output, except grep's exit 1 for no match. |
| `think`, `task_tracker`, others | Outputs. |

A `cat -n` in the shell and an editor view of the same lines are 1 object.
3 signs flag a row as condensed. It has a second `user` message that is not the auto-continue prompt, or a tool result with no matching call. Or it stopped at the iteration cap with fewer assistant messages than the cap.

### Shell commands: `bash.py`

A small scanner splits the command on unquoted `&&`, `||`, `;`, `|`, `&` and newlines.
Quotes and escapes are resolved inside words, so a quoted `|` is a word, not a pipe.
A backslash-newline joins 2 lines. An unquoted word that starts with `#` is a comment up to the newline.
Heredoc bodies are dropped. Leading `VAR=value` words and wrappers such as `timeout 10` are skipped. A `cd` sets the directory.
- **Pure read**: 1 reader on exactly 1 file, maybe piped into filters, with no redirection. Readers are `cat`, `head`, `tail`, `nl`, `wc`, `less`, `more`, `sed` without `-i`, `grep` without `-r`, `rg` and `awk`. The output is a read of that file. `rg` searches directories, so it reads a file only when the operand is a known file or has an extension. An operand ending in `/` is never a file.
- **Writes**: `rm`, `mv`, `touch`, `truncate`, `tee`, the last operand of `cp`, and `sed -i`, which is a reference first. Also `git checkout` or `git restore` of a known file, and `>` or `>>` into a file.
- **References**: every other operand that resolves to a known file.
- **URLs**: `curl` or `wget` of exactly 1 URL, with no output file, is a URL read. Another command that names a known URL references it.

A relative path resolves against the working directory. With no directory, as in our logs, it matches the 1 known path that ends with it.

### Our Copilot logs: `copilot.py`

It reads `~/.copilot/session-state/*/events.jsonl` and reports aggregates only.
Main-agent tool calls are the `toolRequests` of `assistant.message` events with no `parentToolCallId`. Their results are `tool.execution_complete` events.
- `view` is a read. `edit` is a reference, by `old_str`, then a write. `create` is a write.
- `apply_patch` names files in `*** Update File:` (a reference, then a write), `*** Add File:` and `*** Delete File:` (writes).
- `bash` follows the shell rules, with no known directory. A tool with a `url` argument, such as `web_fetch`, is a URL read. Everything else is an output.
- Sub-agent calls are not accesses, but their `fileEdits` still make new versions.
- A `session.compaction_complete` event with `success` is a compaction. Re-reads are split into those in the same compaction window as the previous access and those after a compaction.
- A session that shares at least half its tool calls with a longer session is a fork, and is skipped. 3 of our session logs share 18,005 tool calls. Without this rule they count 1 session 3 times.
- Session ids and object keys are salted with a random value per run, so a hashed path cannot be guessed.

## Sampling

The datasets-server `rows` endpoint returns at most 100 rows per call. We call it sequentially, 1.6 s apart.
A 429 raises that gap by 25%, once per episode. Retries back off 1, 2, 4, 8, 16 and 32 s, or follow `Retry-After`. After 6 retries the run stops.
The request count and the time spent fetching are capped. A row whose `trajectory` cell comes back truncated is skipped and counted.

The sample cuts the rows into equal strata and takes 1 random run of rows per stratum, with seed 87.
SWE-agent rows come grouped by instance: 100 consecutive rows held 8 instances. So SWE-agent takes 1 row from each of 1,000 strata.
OpenHands rows are close to independent: 100 consecutive rows held 99 instances and 85 repos. The API allowed about 15 calls a minute.
So OpenHands takes 10 consecutive rows from each of 100 strata. The bootstrap resamples instances in both.

## Threats to validity

- **A stored trajectory may not be what the model saw.** Issue #87 notes 2 cases. OpenHands 0.54 defaults to a conversation-window condenser, so some late steps may already be condensed in stored data. We flag rows with signs of it. SWE-agent 0.7 drops old observations from each call (`LastNObservations`) without changing the saved history. The dataset card does not say which setting these runs used. If they did, a stored trajectory overstates the context. Some re-reads then happen because the model no longer saw an old output. We treat each trajectory as the reference stream. The scaffold's own policy is 1 more baseline, not ground truth.
- **References are a proxy.** An action that names a file may not need its content, as when it runs a script. And the agent uses outputs it never names: a search result, an error message, a failing test. So we both overcount and undercount real dependencies. Identifiers and error text that appear later are not references here. `--drop-refs` reruns everything on the agent's own re-reads alone, which need no proxy.
- **Versions are coarse.** A write kills every view of the old version, even of lines it did not touch. This undercounts references to unchanged parts of edited files.
- **Content the agent wrote itself** is in its own messages, outside the model. A later use of a file it created references nothing. We count these uses.
- **Unseen writes.** Scripts, `git stash`, `git apply` and installs can change files without a write we detect. A later view with different text is still a new object, but a reference can go to a stale view.
- **Tokens are characters / 4.** Real tokenizers give about 3 to 4 characters per token on code. All policies use the same unit, so comparisons hold. Absolute budgets are approximate.
- **Messages are outside the budget.** A real window also holds the task, the agent's messages and fixed overhead.
- **Objects are whole outputs.** Overlapping windows are separate objects. A finer unit, such as lines, would lower the optimum.
- **Sampling.** SWE-agent has many runs per instance. OpenHands is sampled in blocks of 10 rows. Pooled ratios weight long trajectories more.
- **Our own logs** are 21 sessions from 1 user and 1 harness. They are not a sample of anything.

## Data and license

- [nebius/SWE-agent-trajectories](https://huggingface.co/datasets/nebius/SWE-agent-trajectories), by Nebius, CC-BY-4.0. The dataset card also asks users of the model outputs to follow the Llama 3.1 License.
- [nebius/SWE-rebench-openhands-trajectories](https://huggingface.co/datasets/nebius/SWE-rebench-openhands-trajectories), by Nebius: Trofimova et al., "OpenHands Trajectories with Qwen3-Coder-480B-A35B-Instruct", Nebius blog, 2025. CC-BY-4.0.

We changed nothing in the data. The results are aggregate counts derived from it. Rows are cached under `~/.cache/zerodb-ctxcache/` and never committed.

## Tests

`python3 -m pytest research/ctxcache/tests` runs in under 1 s, with no network and synthetic fixtures only.
- `test_bounds.py`: the bound property on 500 random tiny traces.
- `test_policies.py`: hand-checked results per policy, with reuse distances, peak live and dead tokens.
- `test_adapters.py`: 1 table per adapter, on rows shaped like the real ones.
- `test_fetch.py`: API outcomes against a fake HTTP layer, and the pinned samples.
- `test_shell.py`: 1 table of shell commands and the reads, writes and uses they imply.
- `test_cli.py`: the CLI on the fixture trace.
