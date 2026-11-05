# 0008: One engine, local or hosted

Status: accepted direction, 2026-11-05. Owner picked design A over local-first sync (B) and cloud-only (C).

## Problem

Agents on 1 machine already share 1 store. Every harness reads every agent's notes at chat start. A team or a cloud agent can't reach that store, because clients connect only over a unix socket or `127.0.0.1`. We want 1 model that works on a laptop today and hosted later, without a second product.

## Design

The same `atticdb serve` binary runs in both places. Only the address changes.

```
laptop:  agent -> atticdb mcp -> unix socket      -> atticdb serve -> local disk
hosted:  agent -> atticdb mcp -> https + token    -> atticdb serve -> VM disk
```

- **Same data model everywhere.** `user/` rules, `repo/<repo>/<area>/` decisions, `agent/<agent>/` app notes, `team/` reserved, append-only notes, and file refs.
- **Same 8 MCP tools.** An agent can't tell where the store runs.
- **Tenants are the unit of hosting.** 1 tenant per person or team. Tenants and tokens exist today (`atticdb.json`).
- **Scale out by placing tenants, not by splitting 1 tenant.** Each tenant's store lives on 1 VM with a block disk. Research r6: Azure VMs with Premium SSD v2, about $60/mo for 10 tenants. Container Apps and ACI can't attach block disks.

## What has to change for hosted

1. Clients accept an `https://` URL plus a token from `--token-file`. Plain `http://` stays loopback only.
2. TLS in front of `serve`, with a reverse proxy first, not TLS inside the engine.
3. Scoped tokens per agent (#35) before untrusted clients connect.
4. Rate limits and quotas per tenant.
5. Backup: ship sealed tape segments to object storage. The tape is append-only, so a segment never changes after it seals.

## What this does not do

- No sync between a laptop and a hosted store. That is design B. Revisit when 3 named users ask for a team view.
- No cloud-only store. No embeddings or vector index. No agent in the middle of retrieval.

## Order of work

1. Capture what agents lose at compaction (`PostCompact`), local only. Plan `docs/plans/2026-11-05-compact-notes.md`.
2. `atticdb connect` for Gemini, Antigravity and Claude Desktop (#191).
3. Hosted steps 1 to 5 above, each behind its own issue.
