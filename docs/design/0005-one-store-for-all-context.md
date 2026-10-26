# 0005: Can one store hold all agent context?

Status: research memo. No code, no hardware spend. It answers a feasibility question and sets gates for later work.

## Question

Agent context is more than text. It's files, folders, images, video, links and the LLM's KV cache. Today each lives in its own system, often several hops from the disk. Ceph and MinIO, for example, go from an S3 gateway to an object library to block devices. Can 1 store hold all of it, close to the hardware, consistent and compressed? If not, what blocks it?

## Short answer

- **1 namespace, yes. 1 durability rule, not worth it.** One daemon can own naming, tenancy, quotas and forget for every kind of context. Each class still gets its own data path. KV is recomputable, so syncing it buys no correctness and costs disk bandwidth. We choose an unsynced KV tier. That's a design choice, not a hard limit.
- **Durable text, facts and links:** done today, or planned in 0004 (#73).
- **Files, folders and objects:** feasible. #73 plus folder manifests.
- **Branch-aware memory:** feasible by indexing repo memory files per branch. Git stays the source of truth.
- **KV cache:** feasible only for self-hosted models. What counts is the inference backend, not the harness. A hosted API keeps the KV, so a KV tier does nothing for agents on one. Deferred behind an evidence gate.
- **RDMA and GPU-direct reads:** later. They need GPU hardware we don't have, and the reference design is a prototype.

## Verified prior art

Each row is the data path for 1 read of stored context. "Hop" means a process, protocol or copy boundary.

| System | What it stores | Read path | Durable? | Status |
|---|---|---|---|---|
| vLLM + LMCache + Ceph RGW ([Ceph blog, 2025-12](https://ceph.io/en/news/blog/2025/vllm-kv-caching/)) | KV blocks as S3 objects | GPU ← pinned host buffer (NIXL) ← S3 over TCP ← RGW ← RADOS ← OSD ← NVMe | Ceph object, but used as a cache | Measured: up to 23x lower TTFT, nearly 60 GB/s of S3 traffic from 1 host |
| `rados-nkv` ([Ceph blog, 2026-06](https://ceph.io/en/news/blog/2026/for-whom-the-door-bell-tolls/)) | KV blocks as NVMe key-value pairs | GPU doorbell → QEMU MMIO bridge → SPDK vfio-user NVMe-KV → librados → OSD. Payload DMA goes straight into GPU memory | Ceph object | Prototype. 136 µs for 1 warm 4 KiB GPU-initiated read on AMD, in a QEMU guest. Builds on BaM ([arXiv 2203.04910](https://arxiv.org/abs/2203.04910)). The NIXL plugin is a separate, draft PR ([ai-dynamo/nixl#1753](https://github.com/ai-dynamo/nixl/pull/1753)). It moves data between host DRAM and the KV namespace, not straight into GPU memory |
| llama.cpp server ([README](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)) | 1 slot's KV as a file | `POST /slots/{id}?action=restore` ← file in `--slot-save-path` ← local fs | A plain file | Shipped |
| LMCache local disk or remote connector ([`RemoteConnector`](https://github.com/LMCache/LMCache/blob/dev/lmcache/v1/storage_backend/connector/base_connector.py)) | KV chunks | GPU ← host RAM ← backend (`exists`, `get`, `put`, `list`, batched variants) | Backend decides | Shipped. Backends include Redis, Mooncake, 3FS, S3, Azure and local fs |
| Postgres + Git + SQLite (Yash Agarwal's team, field input below) | Global memory in Postgres. Per-project memory as Markdown in the repo | Agent tool → Postgres. Files → Git → indexer → SQLite | Yes | In production for 1 team |
| zerodb today | Turns, facts, retry keys | Agent → MCP or HTTP on a local socket → connection thread → engine index → `Tape::read` → page cache or NVMe. Reads skip the writer | Yes. Writes sync before ack | Shipped |

What the prior art says:
- **Every KV system treats KV as a cache.** It's evictable and recomputable, and losing it costs a recompute, not data. Some backends are durable anyway, like Ceph objects. Durability there is the backend's property, not something the engine relies on.
- **KV isn't portable.** A block is valid only for the exact weights, dtype, quantization, attention variant and parallel layout. LMCache's `CacheEngineKey` holds `model_name`, `world_size`, `worker_id`, `chunk_hash` and `dtype`. It has no tenant, no weights digest and no quantization. A shared store must add those to the key itself.
- **Hops cost latency, not throughput.** The Ceph S3 path still moves about 60 GB/s from 1 host. The `rados-nkv` GPU prototype removes the host bounce to cut per-IO latency, which matters for many small blocks.

## Hosted versus self-hosted inference

This is the biggest finding, and it's not an engineering problem.
- Claude Code on Anthropic's API, and Codex on OpenAI's, run on hosted models. The provider holds the KV cache. Anthropic's prompt cache is isolated per organization and workspace, lives 5 minutes by default or 1 hour at 2x write cost, and has no export API ([docs](https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching)).
- So a local KV tier can't help a hosted-model agent. The only lever there is sending the same prompt prefix, so the provider's cache hits. That's a prompt-layout concern, not storage.
- A KV tier helps only when the user runs the model: llama.cpp, vLLM or SGLang on their own GPU.
- So eligibility depends on the configured backend, not the harness. Codex can also run local models (`--oss`, with Ollama or LM Studio). Then it depends on whether that engine can save and load KV.

Our first target user is Claude Code on Anthropic's API (0004, Q1). For that user, the KV class adds nothing.

## Bandwidth and capacity model

Assumptions: prefill costs about 2 × active parameters FLOPs per token. GPUs run at 40% of dense peak. H100 is 989 TFLOPS bf16, RTX 4090 is 165 TFLOPS fp16. The model is linear in tokens. It ignores attention's quadratic term, which slows recompute at long context, so real break-evens fall as context grows.

KV bytes per token = 2 × layers × KV heads × head dim × bytes per element. DeepSeek-V3 stores a 576-wide latent per layer (MLA).

| Model | KV per token (fp16) | Hardware | Prefill per token | Load beats recompute above |
|---|---|---|---|---|
| Llama 3.1 8B | 128 KiB | 1× RTX 4090 | 242 µs | 0.54 GB/s |
| Llama 3.1 8B | 128 KiB | 1× H100 | 40 µs | 3.2 GB/s |
| Llama 3 70B | 320 KiB | 8× H100 | 44 µs | 7.4 GB/s |
| DeepSeek-V3 | 68.6 KiB | 8× H100 | 23 µs | 3.0 GB/s |

What the disk gives:
- A PCIe 4.0 x4 NVMe link tops out at 7.88 GB/s.
- Azure L8s_v3: 1 × 1.92 TB NVMe at 2,000 MB/s. L80s_v3: 10 disks at 20,000 MB/s ([Microsoft Learn](https://learn.microsoft.com/en-us/azure/virtual-machines/sizes/storage-optimized/lsv3-series)).

| 100k-token session | fp16 | fp8 | Sessions on 1.92 TB (fp16 / fp8) |
|---|---|---|---|
| Llama 3.1 8B | 13.1 GB | 6.6 GB | 146 / 293 |
| Llama 3 70B | 32.8 GB | 16.4 GB | 59 / 117 |
| DeepSeek-V3 | 7.0 GB | 3.5 GB | 273 / 546 |

Reading the tables:
- **Consumer GPU with local NVMe:** 1 drive beats recompute by about 10x. This is where a local KV tier pays off most, and it needs no RDMA.
- **H100 class:** 1 Gen4 drive is near break-even. It takes 2 to 4 striped drives, or a fast network, to win clearly.
- **Capacity isn't the limit.** 1 disk holds 59 to 546 long sessions.
- The gain for consumer GPUs comes from slow compute, not fast storage. That favors a plain file path over GPU-direct work.

## Data model: 3 classes, 1 namespace

| Class | Holds | Durability | Key |
|---|---|---|---|
| Durable | Turns, facts, refs, folder manifests, branch index | `fdatasync` before ack. Today's tape | LSN |
| Content | Files, chunks of large files, media | File synced before its ref is written (0004, Track C) | sha256 of content |
| Ephemeral | KV blocks, llama.cpp slot files | Never synced. Evicted under a size cap and on forget | Tenant + model fingerprint + the engine's full block key (worker rank and chunk hash included) |

Checked against our invariants and known attacks:
- **Separate data paths.** KV moves GB/s in large blocks. The tape writer is bound by `fdatasync` and 1 thread (D83, D84). KV traffic must never pass through it. The ephemeral class gets its own files and its own threads, and shares only the namespace, tenancy and forget metadata.
- **Tenancy.** KV is keyed per tenant, with no sharing across tenants. Shared prompt caches leak prompts through timing: 7 API providers, OpenAI included, were found sharing caches across users ([Gu et al., ICML 2025](https://arxiv.org/abs/2502.07776)). The cost is losing cross-tenant prefix hits, such as a common system prompt. On 1 machine there's usually 1 tenant anyway.
- **Content dedup across tenants.** 0004 lets 2 tenants share 1 file for identical bytes (Q5), with separate refs. That's safe only while a put always streams and hashes the full body. Add 2 rules to #73: no "does this digest exist" call, and no upload that skips bytes because the client sent a digest. Either one would let a tenant probe for another tenant's files.
- **Model fingerprint.** The engine doesn't supply it, so the connector must. That means a weights digest, dtype, quantization, attention variant and TP and PP layout, set in connector config. A wrong fingerprint returns stale KV. The engine then produces wrong output with no error, so this is a correctness issue, not a perf one. The fingerprint prefixes the engine's full block key and never replaces it. The worker rank stays in, so tensor-parallel shards don't collide.
- **Forget (I10).** KV derived from forgotten turns is derived content. The rule: a forget evicts that tenant's KV and slot files for every model. It's coarse, but KV is recomputable, so the only cost is one slower prefill. Tracking which blocks came from which session is not worth it at our scale.
- **Folders.** A folder is a content-addressed manifest of `{path, mode, digest}` entries, like a Git tree. Versions share unchanged files. Forgetting a folder hides its refs. It doesn't cascade into files other refs still use, which matches how 0004 handles shared bytes.

## Integration surface

Smallest first.

| Path | Work | Who it serves | Keeps the GPU path open? |
|---|---|---|---|
| llama.cpp slot files in the ephemeral class | A small put and get keyed like KV, unsynced, evicted on forget. Reuses #73's streaming code. Not plain objects: those survive a forget of their source turns (0004) | Local llama.cpp users | No |
| LMCache `RemoteConnector` in the Python client | `exists`, `get`, `put`, `list`, `close` over the local socket, plus a fingerprint-and-tenant key prefix | vLLM and SGLang users via LMCache | Partly. LMCache can move to NIXL later |
| NIXL backend plugin | A C++ plugin, as nixl#1753 does for `rados-nkv` | GPU-direct reads | Yes |
| vLLM KV connector directly | Tied to vLLM internals | vLLM only | Yes |

LMCache already ships a local-disk backend. On 1 machine, zerodb's connector adds only tenancy, forget and quotas. That's real for a shared box, but thin for 1 user. It's another reason to wait for a user.

## Compression per class

| Class | Plan | Evidence |
|---|---|---|
| Durable text | zstd per record, behind #74's gate | zstd 1.5.7 `-1` gets 2.9x on Silesia at 510 MB/s ([zstd README](https://github.com/facebook/zstd)). Real ratio on turns comes from dogfood (#46) |
| Content | zstd by MIME type. Skip JPEG, PNG, MP4, ZIP and other compressed types | 0004, Track D |
| Ephemeral KV | Leave it to the engine | fp8 halves bytes by construction. KIVI 2-bit: 2.6x less peak memory, weights included ([arXiv 2402.02750](https://arxiv.org/abs/2402.02750)). CacheGen: 3.5 to 4.3x smaller ([arXiv 2310.07240](https://arxiv.org/abs/2310.07240)). These are lossy and model-aware, so a store can't apply them blindly. Generic lossless on KV is unmeasured |

## Branch-aware memory

Field input, from a call with Yash Agarwal (2026-10-26). His team keeps global memory in Postgres. Per-project memory lives as Markdown in the repo, versioned by Git. A deterministic indexer loads it into SQLite for SQL queries. His concern: zerodb has 1 linear log per tenant. It has no branches, no merge and no diff, so agents on several branches step on each other.

| Option | Review flow | Merge conflicts | Query after compaction | Forget (I10) | Cost |
|---|---|---|---|---|---|
| 1. zerodb branches: a branch is a named pointer to a manifest, merges are 3-way | New. We'd rebuild PR review | We define merge for facts and turns. Hard | Yes | Covered | Large. It rebuilds Git |
| 2. zerodb indexes repo memory files per branch. Git stays the source of truth | Git's, unchanged | Git's, unchanged | Yes, via zerodb queries | Hides index entries. Git history still holds the bytes. That's the user's repo, so it's out of scope | Small. An indexer keyed by repo, branch, path and blob sha |
| 3. Sessions and facts tagged by branch, no merge | None | None. But memory doesn't follow a merged branch | Yes, per branch | Covered | Tiny |

Recommendation: option 2, with option 3's tag as a free first step. Capture hooks (#72) already know the working directory, so they can record the branch on each session. The indexer comes after. It matches how teams already work, and it doesn't compete with Git.

## Verdict

| Context type | Feasible in zerodb? | Path | Gate |
|---|---|---|---|
| Turns, facts | Yes | Shipped | None |
| Links | Yes | #73, C-links | None |
| Files and objects | Yes | #73, C-objects | #45 forget design |
| Folders | Yes | Manifests on top of objects | After C-objects |
| Branch-aware memory | Yes | Option 2, with option 3's tag first | None |
| KV for hosted models | No | Not storable. Provider-side only | None. It's a hard limit |
| KV for self-hosted models | Yes | Slot files in the ephemeral class first, then an LMCache connector | A self-hosted user, plus a measured TTFT win on their hardware |
| GPU-direct and RDMA reads | Not yet | NIXL plugin | The KV tier is in use, a user needs lower per-IO latency, and we have GPU hardware |

Ranked blockers:
1. **Hosted models don't expose KV.** Our first users get nothing from a KV tier. This is a hard limit, not a gap we can close.
2. **No model fingerprint in engine keys.** It's solvable in connector config. A mistake there gives silently wrong output.
3. **Bandwidth at the high end.** On H100-class GPUs, 1 Gen4 drive is near break-even. Consumer GPUs are fine.
4. **Write-path isolation.** KV must never touch the tape writer. That's a design rule, and easy to keep from day 1.
5. **Hardware for GPU-direct.** Our GPU quota is 0. ND A100 v4 is $27.20 per hour and ND H100 v5 is $98.32 per hour. Whether Azure supports GPUDirect Storage is unverified. The `rados-nkv` GPU path is a prototype, and its NIXL plugin is a draft.

New design ideas worth keeping:
- **Forget that reaches derived data.** A forget evicts derived KV and hides object and folder refs, under 1 tenancy model. No system we found does this across classes.
- **Probe-proof content dedup.** Shared bytes are allowed, but there's no existence check and no client-digest shortcut.

## Not in this memo

- Code or benchmarks.
- An S3-compatible API. Rejected in 0004.
- Multi-machine replication.
- Training data.
- Unverified: GPUDirect Storage on Azure, and any `rados-nkv` throughput beyond the single-read latency above.
