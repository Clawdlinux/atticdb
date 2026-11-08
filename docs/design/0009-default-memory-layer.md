# 0009: The default memory layer, local and hosted

Status: proposed, 2026-11-08. The owner picked approach C and the first user below. Builds on [0008](0008-one-engine-local-or-hosted.md), which still holds.

## Why

AtticDB should be the default memory for every agent a person runs. Coding agents come first, other tools later. All of them share 1 context, on a laptop or hosted, through the same MCP tools. Over time the store grows from a pile of notes into organized knowledge, a second brain. The cloud offer is sized from measurements, not guesses.

## First user

A solo developer whose agents run in more than 1 place: a laptop, a desktop, a Codespace and cloud coding agents. Today each machine has its own home store, so the agent on machine 2 starts blank. In week 1 they point every agent at 1 hosted tenant. Their rules and decisions then follow them everywhere. Teams come later.

## Goals

- 1 tenant serves every agent of 1 person, local or hosted, with no change to the agent.
- No write is lost silently when 2 agents touch the same fact.
- Organized knowledge pages, built by the user's own agent, that link back to the notes they came from.
- A price per tenant backed by a measured tenants-per-VM number.

## Non-goals

- Embeddings or a vector index.
- Sync between a laptop store and a hosted store (0008 design B).
- Connectors for Slack, mail or docs.
- Teams before Phase 3.

## Anti-goals

These count as failure even if the code works:
- A second product for the cloud, with its own data model or tools.
- A model inside the store's read or write path.
- A start block so big that agents stop following it.
- Lock-in: data must stay exportable as markdown (`atticdb export`).

## Constraints

- 0008 holds: 1 binary, tenants are the unit of hosting, no model in the store.
- The session-start block stays at most 6,000 characters (`MAX_BLOCK` in `crates/atticdb/src/hook.rs`).
- D76 holds: the model never sees `if_lsn`.
- No new dependencies without an issue. Azure spend must be reported, and VMs deallocated after each run.

## Approaches

| | A: your agent curates | B: we run a curate worker | C: A now, B later as an option |
|---|---|---|---|
| Who runs the model | The user, with their quota | Us, in the cloud | The user now. Us later, opt-in |
| Fits 0008 (no model in the store) | Yes | No | Yes now |
| Our cost per tenant | Disk only | Tokens per tenant | Disk now |
| Privacy | Notes never leave the user's agent | We read the notes | The user's choice |
| Works offline | Yes | No | Yes |

**Picked: C.** The store stays simple and costs us nothing per tenant now. The hosted worker can come later as a paid option that runs the same steps. Anthropic's post on agent memory ([components of agent memory](https://www.anthropic.com/research/components-of-agent-memory)) backs the shape. The "dreaming" pass works best when it can check notes against the raw transcript, and AtticDB keeps that transcript append-only.

## Phase 0: close 2 gaps and measure

Planned in detail in `docs/plans/2026-11-08-phase0-shared-facts-and-capacity.md`.

### 0a. No lost fact writes over MCP

Today MCP `fact_set` is last-write-wins. When 2 agents edit 1 fact, the later write hides the earlier one. The old revision stays on disk, but no agent sees it. The engine already has the fix: `if_lsn` makes a write conditional, and a mismatch is HTTP 409 `conflict` (D76).

Design:
- `atticdb mcp` remembers, per key, the LSN it last saw for that key in this process. It sees an LSN on `fact_get`, on `fact_list` and on each successful `fact_set`.
- `fact_set` sends that LSN as `if_lsn`. For a key this process never saw, it sends `if_lsn: 0`, which means "the key must not exist yet".
- On a conflict, the tool returns an error with the current value, its agent and time. It remembers the new LSN. The agent merges and calls `fact_set` again, and that call succeeds unless another write lands first.
- The model still never sees `if_lsn`. D76 stays.
- The tools are built in `crates/atticdb/src/client.rs`. `fact_get` and `fact_list` already return `lsn` there. `file_ref` already reads before it writes, so it gets the check for free.
- The `fact_set` description gets 1 sentence: if the key changed since you last read it, or you haven't read it yet, nothing is written and you get its current value to merge. `INSTRUCTIONS` stays unchanged, inside its 270-word budget.

Cost: overwriting a fact you haven't read costs 1 extra round trip. That's the point: the agent must see what it replaces.

### 0b. Fact history over MCP

A 9th tool, `fact_history`, over the existing `GET /v1/facts/history`. It returns the revisions, newest first, with agent, time and content, capped at 64 KiB. An agent can then see what changed and who changed it.

### 0c. Tenants per VM

We need a number before we can price the cloud. A fresh home is about 34 MiB on disk (D106), so disk won't be the limit. RAM, file handles and tail latency will be.

Measure 2 layouts:
- **Process per tenant:** 1 `atticdb serve` and 1 data dir per tenant.
- **Shared process:** 1 `atticdb serve` with N tenants in `atticdb.json`.

For each layout, at N = 1, 10, 100 and 1,000 tenants, measure:
- resident memory, idle and under load
- open file handles
- disk per tenant
- p50 and p99 of `memory_append` and `fact_set` with a set share of active tenants

The output is a results doc giving the tenants per VM that hold p99 under a set bound, and the cost per tenant per month. First run locally to shape the harness, then 1 Azure run. The VM size is set in the plan, and spend is reported.

**My call, to be confirmed by 0c:** process per tenant.
- Delete, export and backup of 1 tenant become operations on 1 directory.
- One tenant's poisoned engine or full disk can't stop the others.
- 0008's backup ships sealed segments per tenant.

If 0c shows it costs more than 2x the shared layout per tenant, we switch to shared.

## Phase 1: hosted personal tenant

0008's 5 steps, each behind its own issue: https clients, a TLS proxy, scoped tokens (#35), per-tenant quotas, and segment backup. Plus `atticdb connect --url <https> --token-file <path>` for the 7 harnesses `connect` supports today.

Done when 1 person's agents on 2 machines and 1 cloud agent read and write 1 hosted tenant.

## Phase 2: curate

- AtticDB ships a curate prompt. Any connected agent runs it, by hand or on a schedule.
- It reads raw notes and writes pages as facts under `kb/<topic>`. Each page names the note sequence numbers it came from. D76 reserves `source_refs` on facts for this later.
- The start block shows a short `kb/` index inside the 6,000-character cap.
- Done when an eval shows agents answer better with `kb/` pages than without. That's the handoff eval or LoCoMo, picked in Phase 2's own design.

How the prompt reaches each harness, through MCP prompts or a CLI command, is open until Phase 2's design.

## Phase 3: teams and a hosted curator

Team tenants with a `team/` namespace and scoped tokens per member. An opt-in hosted worker runs the same curate prompt for paying tenants. Pricing comes from the 0c numbers. Phase 3 gets its own design.

## Left to the builder

- Phase 0's exact tool wording and error JSON, kept in the style of the 8 existing tools.
- The 0c harness language and load shape, as long as each run can be repeated with 1 command.

## Calls made after the builder check

These are my calls, not the owner's. Each says what it costs if it's wrong.

1. **0c pass bar: server-side p99 at most 50 ms for `memory_append` and `fact_set`.** The headline uses 5% of tenants active. The results also report 1% and 20%, so the price can move without a rerun. Why 50 ms: a tool call is interactive, and the start hook gets 3 s for everything (`BUDGET` in `hook.rs`). Cost if wrong: the headline count changes, but the sweep already holds the other answers.
2. **Process per tenant stays always on in 0c, with no router.** 0c also measures cold start, the time from spawning `serve` on an existing dir to its first answered request. With that, Phase 1 can price start-on-demand behind its proxy. Cost if wrong: always-on overstates per-tenant RAM, which biases toward the shared layout. The 2x rule then compares honestly against the worst case.
3. **The conditional write is for `atticdb mcp` only.** The LSN memory lives in the client but is turned on only by `mcp`. The shell verb `atticdb fact set` stays last-write-wins. It's a fresh process each run, run by a person on purpose. Cost if wrong: a script using the shell verb can still overwrite. It could get a flag later.
4. **Facts in the start block count as unread.** The hook runs in a separate process, and passing LSNs through the block would show them to the model, against D76. A first correction of such a fact costs 1 extra round trip. The conflict error says plainly that it isn't a failure: "this fact changed, or you haven't read it in this chat yet. Nothing was written. Its current value is below. Merge it with your change and call fact_set again." The `fact_set` description says the same. Cost if wrong: 1 extra call per first correction.
5. **Hosted and 0c tenant dirs use `segment_mib: 16`, as homes do.** That's about 34 MiB per tenant in the process-per-tenant layout. The shared layout has 1 tape for all tenants, so its disk per tenant is close to its data size. The results doc reports disk per tenant for both layouts. Cost if wrong: none for 0c, since both sizes can be measured.

Also from research: a 2026-11-01 profile found `serve` uses 1 thread per connection, about 36 µs per append at 128 writers (draft #151). The shared layout at 1,000 tenants will hit this first. 0c reports the thread count, so we can see it.
