---
title: "Acks: what a durable write means"
description: Why AtticDB acks a write only after fdatasync.
date: 2026-01-15
author: AtticDB team
tags: durability, fsync
---

A write is acked only after it is synced to disk.

## Why it matters

See [the MCP tools](docs:mcp) and [how it works](docs:how-it-works#group-commit).

<script>alert(1)</script>
