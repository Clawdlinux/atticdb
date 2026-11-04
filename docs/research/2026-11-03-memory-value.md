# Where AtticDB Can Prove Memory Value

Research date: 2026-11-03. AtticDB baseline: `b2e07c3`.
Worktree branch: `research/memory-value-study`.
Status: research and a provisional benchmark contract. No runtime change or live model benchmark was authorized or executed.

## Recommendation

Start with evidence-backed Claude Code/Codex handoffs, including rejected approaches and changed constraints.
Keep single-harness compaction and enterprise learning as separate evaluation tracks.

Do not lead with "persistent memory." All three investigated harnesses already provide it.
Do not equate faster durable appends with better coding accuracy.
First establish where the memory pipeline fails, then change that stage.

The strongest current findings are narrower than a product-wide accuracy claim:

- Existing setup runs expose missed capture and retrieval calls.
- The setup scorer accepts at least one deliberately wrong historical explanation.
- Structured fact provenance remains unavailable in AtticDB's current API.
- Native memory, maintained files, temporal graphs, and shared learning loops are serious alternatives.

## What Harnesses Already Retain

These are documented capabilities, not results from equivalent live benchmark runs.
Live documentation was accessed on 2026-11-03. Defaults and availability can change between releases.

| Harness | Existing persistence and recall | Context boundary | Important limit |
| --- | --- | --- | --- |
| Claude Code | Project instructions; machine-local Markdown auto memory; topic files read on demand | Compaction summarizes conversation. Root instructions and auto memory reload; scoped rules reload on demand | The startup memory index is capped at 200 lines or 25KB. Saving and reading useful content remain model-dependent |
| Codex | SQLite-backed memory coordination and extraction records; generated Markdown summaries and memory artifacts | Background extraction and consolidation are distinct from the running context window | Native memories are off by default. Idle time, quotas, session eligibility, and external-context settings affect generation |
| Hermes | Bounded MEMORY.md/USER.md; SQLite WAL session history with FTS5 search; optional external providers | Startup memory is frozen. Compaction retains active context while historical rows and lineage remain separately stored | Profiles can select different stores. Tool calls, approval staging, review scheduling, and snapshot freshness affect recall |

Sources: [H1], [H2], [H3], [H4], [H5].

Claude's instructions are guidance, not enforcement. Hooks or permissions handle actions that must be blocked regardless of model behavior.
Hermes also documents post-turn background review, approval controls, model routing, and review token budgets.
Describing it as manual foreground memory alone would be inaccurate.

Codex's source documents bounded extraction jobs, leases, retry backoff, and serialized consolidation.
Extraction calls a model; consolidation can invoke a restricted internal agent.
An immediately fresh memory store is therefore not guaranteed after every chat.

Resume, compaction, durable storage, and cross-session retrieval are different mechanisms.
Persisting a transcript does not mean its relevant contents reach a later decision.
Losing active context does not necessarily mean the original evidence was deleted.

## Backends And Learning Mechanisms

| System | Verified storage architecture | What makes it more than storage | Boundary of this review |
| --- | --- | --- | --- |
| AtticDB | Local append log; revisioned facts; derived lexical search index | Small startup block, explicit MCP retrieval, durable acknowledgement, conditional fact updates | No demonstrated general task-accuracy gain; structured source_refs must remain empty |
| claude-mem | SQLite session storage, WAL plus NORMAL synchronous; local embedding model support | Captured observations and recall infrastructure | Current source says it moved off Chroma. Not every vector or remote-serving path was audited |
| context-mode | SQLite FTS5/BM25; WAL plus NORMAL synchronous | Tool-output routing and sandboxed processing reduce context exposure | Output reduction is not automatically correctness improvement; retention depends on the selected store path |
| Mem0 OSS | Configurable vector store; documented Qdrant default; SQLite change history; optional graph configuration | LLM extraction and updating, embeddings, retrieval, optional reranking | Specify the deployment. There is no single universal Mem0 database or durability policy |
| Graphiti | Pluggable Neo4j, FalkorDB, or Neptune; deprecated Kuzu driver still present | Episode provenance, temporal invalidation, semantic/keyword/graph retrieval | Framework requires surrounding service operations and access controls |
| Zep | Current Graphiti README names Zep's Konig graph service | Managed enterprise context and governance | Do not describe Zep as simply hosted Neo4j or equate it with Graphiti |
| Letta Code | Local JSONL transcripts and Git-tracked MemFS; separate cloud backend | Memory blocks, conversation search, skill learning, periodic dreaming | Old V1 server is archived. Cloud database internals were not verified |
| Entelligence | Shared World Model and trace ingestion; underlying database not established | Review-to-learning provenance, approval, targeted delivery, and holdout feedback | Broader team-learning product; vendor results were not independently reproduced |

Sources: [C1], [C2], [C3], [C4], [C5], [C6].

Database names do not establish extraction quality, retrieval quality, or instruction following.
Neither SQLite nor a vector database is inherently the accuracy bottleneck.
Temporal provenance and cross-tool memory already have competing implementations. They are not exclusive ideas.

### A Real Durability Distinction

SQLite documents WAL/NORMAL as consistent across failures, with possible committed-transaction rollback after power loss or system crash.
Application crashes are different. WAL/FULL adds commit synchronization for durability. See [D1].

The inspected claude-mem and context-mode connection helpers select WAL/NORMAL.
That is a configuration difference, not evidence that either product lost data in this investigation.
Compare both shipped defaults and matched durable settings, including SQLite WAL/FULL.

AtticDB's durable acknowledgement is useful only after the relevant write happens.
It does not prove correct extraction, useful recall, replication, or recovery after device loss.
Checksums detect specified corruption classes; they do not prove semantic truth or protection against a compromised host.

## Bottleneck Evidence

### 1. The Existing Accuracy Measurement Is Too Weak

Parsed three checked-in setup CSVs using the standard CSV parser:

| Check | Recorded result | What it establishes |
| --- | --- | --- |
| Initial Codex | All six rows have zero memory calls; S1 still passes | Injected startup context can work without a retrieval call |
| Codex wording-v2 S2 | 3/3 pass, each with a memory_search call | Small integration check for historical-search invocation |
| Codex wording-v2 S4 | 2/3 pass; the failed row has zero memory calls | Capture can be skipped entirely |
| Claude wording recheck | 8/8 pass | Small integration check under the recorded setup |

The harness used default models rather than explicitly pinned model identifiers.
Its empty-store controls do not establish superiority over native memory or maintained files.
These are not reliable population-level accuracy estimates.

The S2 scorer requires a memory_search call and any accepted answer marker, including `p99`.
A local synthetic counterexample passed with this deliberately wrong answer:

> We dropped Redis because its license forbids reporting p99 metrics.

No model was called for that check. The existing scorer was invoked directly.
This establishes a grading limitation, not an observed model response.
Replace keyword-only success criteria before making factual-accuracy claims.

Local evidence: `research/setup_eval/scenarios.py`, `research/setup_eval/README.md`, and the dated result CSVs.

### 2. Capture And Invocation Are Demonstrated Failure Classes

The recorded runs include failures before useful memory could be loaded or saved.
Hermes independently documents claimed saves without memory-tool calls, staged writes, wrong profiles, and frozen snapshots.
These findings justify instrumentation. They do not prove a dominant failure percentage across harnesses.

Zero retrieval calls are not always failures: correctly injected context can remove the need for a call.
Check whether the required evidence reached the decision, not whether a tool counter increased.

### 3. Provenance And Freshness Are Concrete Gaps To Test

At the AtticDB baseline, `crates/atticdb-core/src/machine.rs` rejects nonempty fact source_refs.
Conditional writes already use revision LSNs; they should not be reinvented as a new feature.
Current search is indexed lexical matching, not the older unindexed scan described in historical plans.

Test whether paraphrases, changed constraints, versioned failures, and revoked guidance are handled correctly.
Do not assume embeddings or a graph will solve these failures before measuring them.
Plain-text notes can carry reasons today, but that is not validated structured provenance.

### 4. Existing Storage And Cache Results Answer Other Questions

The storage benchmarks measure append/read behavior under their documented hardware and durability conditions.
Real-transcript ingestion does not establish that an agent later retrieves and follows the right instruction.

The ctxcache study measures offline headroom on fixed traces and inferred access proxies.
It explicitly warns that changing context would change the agent's later trace.
Its "dead token" and oracle results are not deployable-policy accuracy or token-saving guarantees.

Sources: `bench/RESULTS.md`, `research/ctxcache/results/README.md`, and existing storage benchmark plans.

## What Public Reports Suggest

Refreshed through authenticated Browser MCP. Reports were not reproduced as product defects.
Three X items are replies to our own posts, so they are solicited feedback, not independent demand validation.

| Report | Concrete signal | Benchmark case |
| --- | --- | --- |
| [Huilong](https://x.com/098uyiubgvgcss/status/2117135691734032802), Nov 2 | Link the failed attempt; "failed on v1" must not become "never try again" | Source support, version scope, and changed applicability |
| [Wise Wong](https://x.com/killthewhys/status/2116742196884062634), Nov 1 | Preserve reasoning and know when to revisit rejected options | Supersession and stale-rule rejection |
| [Yosef Shammout](https://x.com/Yosef_Shammout/status/2116741062278443362), Nov 1 | Verify retrieval and following after compaction | Evidence delivery and constraint compliance, separately from task completion |
| [ZeroMark](https://x.com/zeromarkapp/status/2116937061169258809), Nov 1 | Switching Claude/Codex loses context; HANDOFF.md is the proposed fix | Maintained handoff file as a competitive baseline |
| [Shared-repo operator](https://old.reddit.com/r/ClaudeAI/comments/1xhq021/i_run_my_whole_business_from_one_shared_repo_and/) | Reports 88 waiting cards despite an empty-queue answer; rules drift into project settings | Read the authoritative queue; portable guidance across tools |
| [Claude Desktop MCP report](https://old.reddit.com/r/ClaudeAI/comments/1xkmgsd/claude_cant_update_memory_if_mcp_is_in_use/) | Built-in memory reportedly refuses updates with a custom MCP enabled | Platform eligibility and permissions; not a Claude Code storage conclusion |

The queue incident is particularly important: the authoritative records already existed.
A new memory database is not necessarily the remedy. Better source selection or tool access may be sufficient.

Unauthenticated Reddit reads returned login pages; authenticated browser reads succeeded.
No outreach, comments, votes, or promotional posts were made during this study.

## Provisional Benchmark Contract

The clarification response said the owner was unavailable and requested autonomous work.
Cross-harness coding handoffs are therefore the provisional first specification, not an approved runtime roadmap.
Paid execution, deployment, and refactoring remain out of scope.

### Separate Three Questions

1. Storage: does an acknowledged record survive the specified fault, and what does that guarantee cost?
2. Memory: does the right supported evidence reach a later coding decision within the budget?
3. Enterprise learning: does approved experience reduce repeated defects without leaking or retaining unauthorized data?

Do not combine these into one "memory accuracy" score.

### Baselines

| Arm | Configuration |
| --- | --- |
| Native default | Current released harness, documented defaults, native transcript access, and normal repo files |
| Native memory enabled | Explicitly enabled native memory, documented eligibility and settling time, no artificial disabling |
| Maintained files | Native memory plus a concise shared handoff and decision log, with recorded maintenance cost |
| AtticDB added | Same harness and source access, AtticDB's shipped integration, no manually rescued missing notes |
| Representative alternative | One comparable local memory integration; broader graph/team systems only in a compatible workload |

Run same-harness sessions, compaction, process restart, and cross-harness handoffs as distinct conditions.
Keep both Claude-to-Codex and Codex-to-Claude directions visible.
Give every arm the same historical source information and working repository.
Use each product's normal capture path for the end-to-end comparison.
Use identical gold notes only in a separately labeled component experiment.

Pin harness releases, model IDs, provider, reasoning configuration, prompts, tool permissions, and memory settings.
Record operating system, filesystem, hardware, corpus hash, clocks, and index readiness.
Do not use hidden private chat state, unrelated hooks, or background writes from concurrent sessions.
Predeclare timeout and error handling; do not silently drop failed runs.

### Cases And Outcomes

- Recover a decision's reason after it leaves the startup block.
- Avoid a rejected approach while the original constraint still applies.
- Reconsider that approach after a supported constraint change.
- Retrieve evidence under a paraphrased query or changed technical name.
- Distinguish similar projects, branches, people, and timestamps.
- Abstain when evidence is absent, contradictory, inaccessible, or forgotten.
- Preserve a behavioral constraint even when ordinary task tests still pass.
- Reject poisoned memory as instructions and respect current permissions.

Use executable task tests and independently checked constraints as primary coding outcomes.
Check source IDs, quoted support, timestamp, and applicability for memory assertions.
An existing citation alone does not establish that it supports the claim.
Do not infer model use from citation text alone; inspect actions and compare controlled evidence interventions.

### Diagnose The Failing Stage

| Observation | Discriminating check | Smallest justified next step |
| --- | --- | --- |
| Source never captured | Inspect the actual stored event and write receipt | Repair capture or eligibility; not the retriever |
| Captured but not durable | Reopen after a specified fault against an acknowledgement ledger | Repair the persistence boundary |
| Stored but not queried or delivered | Repeat with forced retrieval or budgeted relevant injection | Improve tool routing or delivery timing |
| Queried but relevant evidence missing | Compare retrieval against labeled evidence and oracle context | Test lexical/hybrid retrieval or reranking |
| Correct evidence loaded, wrong claim or action | Supply oracle evidence with the same budget | Investigate selection, reasoning, or enforcement |
| Old guidance overrides a valid update | Inspect source versions and applicability | Fix update and invalidation behavior |
| Fresh authoritative state ignored | Remove stale summaries and require live source access | Fix source priority or tool access |

These interventions locate bottlenecks; oracle results are not deployable-system scores.
Do not collect private chain-of-thought. Tool events, evidence payloads, observable actions, and artifacts are sufficient.

### Measure A Frontier

| Dimension | Metrics |
| --- | --- |
| Coding outcome | Task/test success, constraint violations, repeated-defect rate, unsupported-answer and abstention rates |
| Capture | Eligible-source coverage, false captures, write receipt rate, time until a new fact becomes available |
| Retrieval | Evidence Recall@K/MRR, supported-source precision, stale retrieval, scope violations, delivered evidence tokens |
| Speed | Capture acknowledgement, cold/warm query p50/p95/p99, compaction/recovery delay, complete task latency |
| Cost | Extraction, consolidation, embeddings, reranking, reads, caching, failed attempts, and human maintenance |
| Resources | Idle and peak RSS, CPU, disk growth, index-build time, background work |
| Durability | Lost acknowledged records, corruption detection, uncertain-write treatment, replay correctness, recovery time |

Account for capture and background work, not only the cheap foreground search.
Report readiness both for immediate handoffs and after each system's documented background settling period.
Let speed/accuracy operating points expose tradeoffs rather than choosing a favorable single number.

Use paired task comparisons, held-out tasks and repositories, chronological splits, and predeclared scoring.
Separate exploratory tuning from the final test set.
Use a power calculation for final sample size; repeated toy runs are an integration pilot, not statistical proof.
Cluster intervals by independent task or repository when repeated runs share the same history.
Calibrate model-assisted judges against blinded human checks; prefer executable outcomes where possible.

### Reuse Existing Evaluation

[LongMemEval](https://github.com/xiaowu0162/LongMemEval) provides temporal updates, abstention, evidence labels, and oracle retrieval.
[LongMemEval-V2](https://github.com/xiaowu0162/LongMemEval-V2) adds workflow knowledge, environment gotchas, and premise awareness.
V2's 451 curated questions span web and enterprise trajectories, with a public accuracy-latency evaluation contract.
Its released Codex baseline uses v0.117.0, not the current native-memory implementation.

Reuse those protocols rather than inventing another favorable toy QA benchmark.
Neither replaces a coding-handoff evaluation with native memory and maintained files.
No LongMemEval scores were produced in this investigation.

### Durability And Enterprise Gates

Reuse existing storage harnesses where their fault models and workloads fit.
Separate application kill, OS/power-loss simulation, disk-full, sync errors, corruption, and device loss.
An acknowledgement ledger must survive outside the process under test.
List filesystem and hardware assumptions. Never present SIGKILL alone as a power-loss test.

Compare native configurations and matched guarantees separately.
For SQLite, include WAL/FULL; for other engines, document acknowledgement and persistence policies.
Match transports, payloads, batching, readers, concurrency, and durable-write semantics before interpreting throughput.

For enterprise evaluation, first establish repository count, ingest volume, concurrent writers, retention, and access boundaries.
Then test permission filtering, revocation, conflict resolution, restore, deletion propagation, and tenant isolation.
Measure cross-tenant leakage and stale-authority use as failures, not ordinary recall misses.
AtticDB logical forgetting is not physical purge. The baseline still distinguishes those operations.

A synthetic many-writer test is a capacity point, not proof of enterprise suitability.
Local fsync durability is not replication or disaster recovery.
Team repeated-defect studies need a holdout that avoids shared-learning contamination and model-version drift.
Count substantive defects per comparable opportunity, with stable review effort and human maintenance included.

## Differentiation And Stop Rules

Candidate positioning: local, cross-harness decision memory with inspectable evidence and explicit persistence semantics.
That is a hypothesis, not a claim that the full evidence lifecycle ships today.

The adoption proof is fewer repeated corrections or safer handoffs at a competitive total cost.
The engineering proof is a stage-by-stage account of capture, acknowledgement, retrieval, support, freshness, and outcome.
The enterprise proof adds access control, lifecycle, recovery, and a genuine repeated-defect comparison.

If maintained files match the outcome at lower cost, prefer them or narrow AtticDB's role.
If oracle context still fails, a new database or retriever is not the first fix.
If only paraphrase retrieval fails, test a derived hybrid index before considering a graph rewrite.
If only capture fails, repair capture before optimizing query latency.
Do not promise to win every dimension. Require a useful accuracy-cost-latency frontier under the target workload.

## Limits And Open Decisions

- No live harness experiments, paid benchmark runs, cloud deployment, or runtime changes were performed.
- Current released versions and model configuration must be frozen before evaluation execution.
- Enterprise volume, deployment topology, access controls, retention, and recovery targets remain unspecified.
- No independent reproduction of competitor accuracy or throughput claims was attempted.
- Entelligence reports 53% fewer substantive review comments. Its underlying study data were not available here.
- Its stated 412 comments and reported rates imply different totals, approximately 973. Denominators need clarification.
- That discrepancy is not proof of a false result. Do not copy the figure as validated evidence.
- Its injected-context cost is not established end-to-end operating cost.
- The public feedback sample is selected and small. It does not establish market size or failure prevalence.
- This study does not select an embedding model, graph database, enterprise architecture, or implementation roadmap.

## Primary Sources

Repository heads below are research snapshots, not claims about installed or tagged releases.
Some earlier file reads have separate blob IDs recorded in `findings.md`.

- [H1] Claude Code memory: https://code.claude.com/docs/en/memory
- [H2] Claude Code compaction boundaries: https://code.claude.com/docs/en/context-window.md
- [H3] Codex memory controls: https://developers.openai.com/codex/memories
- [H4] Codex pipeline: https://github.com/openai/codex/blob/61635845491fd720bb49909466aa19fe5633c773/codex-rs/memories/README.md
- [H5] Hermes memory: https://hermes-agent.nousresearch.com/docs/user-guide/features/memory
- [H5] Hermes storage snapshot: https://github.com/NousResearch/hermes-agent/blob/c314285f077e91937c995d419b9fa40af9eb4981/website/docs/developer-guide/session-storage.md
- [C1] claude-mem connection settings: https://github.com/thedotmack/claude-mem/blob/02584c4ac9d92fea45adf5c61e22c546b5738621/src/services/sqlite/connection.ts
- [C1] claude-mem embedding models: https://github.com/thedotmack/claude-mem/blob/02584c4ac9d92fea45adf5c61e22c546b5738621/src/services/vector/models.ts
- [C2] context-mode storage helpers: https://github.com/mksglu/context-mode/blob/3afa9f978a9a1dc6629fd0001c7bda403c48a6e7/src/db-base.ts
- [C2] context-mode retrieval overview: https://github.com/mksglu/context-mode/blob/3afa9f978a9a1dc6629fd0001c7bda403c48a6e7/.cursor-plugin/README.md
- [C3] Mem0 configuration: https://github.com/mem0ai/mem0/blob/753496cf1435d76c3a711f3af51303f7fdcb6420/mem0/configs/base.py
- [C3] Mem0 SDK configuration: https://github.com/mem0ai/mem0/blob/753496cf1435d76c3a711f3af51303f7fdcb6420/skills/mem0/client/python.md
- [C4] Graphiti and Zep architecture: https://github.com/getzep/graphiti/blob/7e0342a49ce5a04f6415ac2f64ce150bb81b0689/README.md
- [C5] Current Letta Code: https://github.com/letta-ai/letta-code/blob/9df436cffa588f45aadef86f34d43fb8f3008c40/README.md
- [C5] Letta local transcript search: https://github.com/letta-ai/letta-code/blob/9df436cffa588f45aadef86f34d43fb8f3008c40/src/backend/local/transcript-search.ts
- [C6] Entelligence learning loop, 2026-09-30: https://entelligence.ai/research/self-improving-coding-agents
- [D1] SQLite synchronization guarantees: https://www.sqlite.org/pragma.html#pragma_synchronous
- [E1] LongMemEval-V2 protocol: https://github.com/xiaowu0162/LongMemEval-V2/blob/2cc8c540bdb87fe6761629b585e727e1c4704520/README.md
- [E2] Anthropic context engineering, 2025-09-29: https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents