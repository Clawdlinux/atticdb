---
title: Group commit & the <tape>
description: How queued writers share 1 fdatasync & why.
date: 2026-03-02
author: AtticDB team
tags: storage, group commit
image: /blog/group-commit/cover.svg
---

Queued writers share 1 sync.

## How it works

![Two writers, one sync](/blog/group-commit/diagram.svg)

```sh
atticdb doctor
```
