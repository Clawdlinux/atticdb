---
title: Our agent memory raced SQLite and Redis. It won on cloud disks and lost on reads.
description: What "saved" really means, where our tiny agent memory beats SQLite and Redis, where it loses, and how to try it in a few minutes.
date: 2026-11-03
author: Shreyansh Sancheti
tags: agents, databases, benchmarks, open source
image: /blog/we-raced-sqlite-and-redis/goldfish-after-compact-og.png
---

You spend an hour with your coding agent. You agree that tests run with `make test`, never `npm test`.
Next morning you open a new chat. It runs `npm test`.

Or you type `/compact` to free up room. The summary drops the 1 detail that mattered.
A Claude Code bug report put it well: "the user must re-explain work that was already done."

![A goldfish in a bowl asks "Hi. Have we met?" next to a sticky note of rules agreed yesterday.](/blog/we-raced-sqlite-and-redis/goldfish-after-compact.svg)

*Drawn for this post.*

This post is about the thing we're building to fix that. It's also about a race we ran and only half won.
I'll keep it friendly for beginners. If you've tuned databases before, skim to the tables.

## What "agent memory" means

The model behind your coding app keeps nothing between chats.
Each new chat starts from what the app hands it. That's your rules file and maybe a summary.
Anything not in there is gone.

Agent memory is a place outside the chat where that stuff lives.
Rules, like "use `make test`". Notes, like "we picked Postgres on Tuesday, and here's why".
When a new chat starts, the app loads the important bits back in.

AtticDB is that place, on your own disk. It's 1 small Rust program, about 1.3 MB.
Your coding apps talk to it through MCP, the plug-in standard they use for tools. It's free and needs no account.

## "Saved" should mean saved

Most of this post is about speed. But speed only counts if the save is real.

When a program writes a file, your computer usually doesn't put it on the disk right away.
It parks the data in memory and writes it out a bit later. That's faster.
It's also how you lose data. Pull the plug in that gap and your "saved" note is gone.

A program can ask the operating system to push the data all the way down.
Then it waits until the disk says "done". Database people call this fsync. You can forget the word now.

AtticDB only says OK after the disk says done. Every write.
If your laptop dies right after AtticDB confirms a rule, the rule is there when it boots.
We test this with simulated crashes and power-loss tests that run every night.
Windows is a bit weaker here, since it can't sync a folder. The README says so.

![2 panels. Left: a program says OK first, the plug is pulled, the note is gone. Right: the program waits for the disk, then says OK, and the note survives.](/blog/we-raced-sqlite-and-redis/pull-the-plug.svg)

*Drawn for this post.*

The catch: waiting for the disk is slow. So the real game is waiting as little as possible.
That's what the race is about.

## The race, part 1: saving notes

The benchmark also included Valkey and DiceDB. Here we compare AtticDB with SQLite and Redis while saving small notes.
Each one confirms a note only after it's really on disk.
Redis was set to sync every write to its log. SQLite ran in its safest mode.
So nobody got to cheat by skipping the disk.

We ran it on a cloud VM with 2 kinds of disk. One is a cloud disk, which sits on the network.
The other is a fast local SSD, like the one in your laptop.
We ran 7 rounds. Each round ran every system once before the next round started.

**Cloud disk.** Confirmed saves per second. Higher is better.

| Writers at once | AtticDB | SQLite | Redis |
|---|---|---|---|
| 1 | 1,167 | 1,143 | 466 |
| 16 | 9,328 | 1,421 | 5,151 |

With 1 writer, the typical save took 817 µs for us. SQLite took 735 µs and Redis took 2,136 µs.
µs means millionths of a second. So SQLite's typical save is a hair faster than ours.
But with 16 writers we're far ahead of both.

![A snail race on a cloud disk with 16 writers. AtticDB leads at 9,328 saves per second, Redis is mid-track at 5,151, SQLite is near the start at 1,421.](/blog/we-raced-sqlite-and-redis/write-race.svg)

*Drawn for this post.*

We win here because of disk trips. On a cloud disk, every trip crosses a network. Trips are what cost you.

Redis adds each write to the end of its log file. So the file grows on every save.
A growing file means the disk must record 2 things: your data and the new file size.
So growing Redis's log costs about 2 device trips per save.
AtticDB preallocates its files. It makes them full size up front, filled with zeros.
The size never changes, so each queued group pays 1 trip.

We also batch. When 16 writers are waiting, we save all their notes in 1 disk trip.
Then we confirm them all together.

SQLite lets 1 writer in at a time. The others wait and retry.
That's a sensible design for a database that lives inside your program.
Its typical wait stays low. Its slowest wait can get ugly, though.
Take 16 writers on the cloud disk.
Across 7 rounds, the median of each round's slowest SQLite save was 46,787,988 µs. Count the digits.
That's seconds, not millionths.

**Fast local SSD.** Confirmed saves per second. Higher is better.

| Writers at once | AtticDB | SQLite | Redis |
|---|---|---|---|
| 1 | 9,799 | 19,410 | 8,110 |
| 16 | 48,611 | 24,936 | 44,572 |

With 1 writer on a fast SSD, SQLite beats us easily. It runs inside your program, so there's no trip over a socket.
With 16 writers we pull ahead.
Push past 32 writers and Redis passes us. At 128 writers it's about 80k a second against our 68k.
That one's on our list as #52.

Our first version of this race wasn't even fair. To us, I mean.
Redis got a dedicated benchmark tool and we got our own client.
With those clients, AtticDB had the highest p99 latency at 16 writers.
p99 means 99% of requests finished within that time. Only the slowest 1% took longer.
Now every server gets the exact same client.
In the run above, AtticDB also went first in every round. So any warm-up slowness landed on us.
The script shuffles the order now.

## The race, part 2: reading notes

Saving is half the job. The agent also reads. It grabs the latest notes, looks up a rule by name, or searches for a word.
So we raced reads too, on an Apple M4 Mac.
We loaded 512 MiB, about half a gigabyte, of real Claude Code and Copilot chats. That's 200,494 notes.
Redis ran with saving to disk off for this one.

**Typical read time, in milliseconds. Lower is better.**

| Read | AtticDB | SQLite | Redis |
|---|---|---|---|
| Latest 20 notes | 0.135 to 0.137 | 0.020 | 0.031 to 0.034 |
| 1 rule by name | 0.021 | 0.004 to 0.005 | 0.007 |
| Word search | 2.09 to 2.13 | 0.49 to 1.05 | not tested |

The ranges are there because we ran each test more than once.

AtticDB comes third on latest notes and rule lookup. SQLite also beats us on search. Redis wasn't tested there.
And that's after a round of fixes.
Before the engine fixes, with connection reuse already enabled, latest notes took 0.256 to 0.268 ms.

The dumbest bug was ours. An earlier Python client opened a new connection for every read.
Imagine calling a friend, asking 1 question, hanging up, and calling back for the next one.
We fixed that separately.

![A podium for latest notes and rule lookup. SQLite on 1st, Redis on 2nd, AtticDB on 3rd holding a sign that says "Happy to be here."](/blog/we-raced-sqlite-and-redis/reads-podium.svg)

*Drawn for this post.*

Here's why we still lose. It isn't magic on their side. It's where each one lives.

SQLite is a library. It runs inside your program, so a read is just a function call.
Redis keeps everything in RAM. A read is grabbing something off your desk.
AtticDB is a separate program. Every read goes over a local socket as an HTTP request with JSON, and back.
That trip alone costs about 17 to 20 µs.
Every record we return has its checksum verified, including records served from the memory cache.
A flipped bit on disk can't sneak into your agent's head.

A latest-notes read takes 0.135 ms. Whether you notice that depends on the rest of your app and model.
We still want to close the gap. Running AtticDB inside your program, the way SQLite runs, is on the list.

## What we fixed: search

Search was our weakest number. With 512 MiB of notes, search p99 was 602 ms.
So 99% of searches finished within 602 ms. That's over half a second to find a word.

The reason was simple. To find "postgres", we read every note and checked it.
That's like finding a word in a book by reading the whole book.

The fix is the index at the back of a book. You look up the word and get page numbers.
Then you only flip to those pages.
We now keep a list of every word and which notes contain it.
A helper builds it in the background, so saving never waits on it.

We also compress notes on disk now. On real chats that saves 62.1% of space.
With default compression, search p99 dropped from 602 ms to 19 to 20 ms.
Without compression, it was 9.8 to 10.2 ms.
On a bigger 2.5 GiB store, it went from 4,111 ms to 29 ms.
The index isn't free. It takes 70 MiB of memory at 512 MiB of notes.
These figures use a different benchmark from the read table. That benchmark measured search p99 at 15 to 18 ms.

The part I like most: the index can make search slower, but never wrong.
It only suggests notes. We still open every suggested note and check it with the exact same test as before.
Notes the index hasn't caught up on yet get read the slow way.
If the index breaks, search falls back to reading everything.
A test compares index search with a full read, page by page. They must match.

## What this means for you

All 3 are good tools. They're good at different jobs.

| You want | Pick |
|---|---|
| A database inside your own app, with the fastest reads | SQLite |
| A very fast shared store or cache for your services | Redis |
| Your coding agent to remember rules and decisions across chats and apps, saved for real, on your laptop | AtticDB |

If you want to build your own agent memory, SQLite is a great place to start. I mean that.
And Redis can save every write to disk too. That's how we tested its writes.

AtticDB isn't for you yet if you need semantic search, production use, or sync across machines or a team.

## Try it in a few minutes

On macOS or Linux:

```sh
curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh
atticdb setup
```

If your shell says `atticdb: command not found`, run it as `~/.local/bin/atticdb` instead.
Or add `~/.local/bin` to your PATH.

`setup` finds Claude Code, Codex, Copilot CLI and VS Code. It asks which ones to connect.
It backs up every file before it edits it.
`atticdb setup --dry-run` shows the plan first and changes nothing.
Codex asks you to approve the memory loader once.
On Windows, `install.sh` doesn't run. Grab the zip from the [latest release](https://github.com/Clawdlinux/atticdb/releases/latest) instead.

Now save a rule. Run this inside your repo, and swap `myrepo` for your repo's folder name:

```sh
atticdb fact set --key repo/myrepo/tests --content "Run tests with make test. Never npm test."
```

Open a new chat in any connected app. Ask it: "What are this repo's rules?"
It should answer from memory, without you pasting anything.

To see what your agent sees, run this:

```sh
atticdb studio
```

It opens a read-only page in your browser. The "Chat start" screen shows the exact memory block your agent gets.

To undo it, `atticdb disconnect <app>` removes exactly what `setup` added.

## Help us make it better

We want to hear where it broke for you. In particular:

- How long did setup take? Where did you get stuck?
- Tell us if your agent used its memory without being asked.
- What should it remember that it doesn't?

[Open an issue](https://github.com/Clawdlinux/atticdb/issues/new/choose) for a bug, an idea or a question.
Or add a comment to [#129, "Tell us what you need"](https://github.com/Clawdlinux/atticdb/issues/129).
A thumbs-up on an existing issue counts too. We build what people ask for.

If you want to contribute code, read [CONTRIBUTING.md](https://github.com/Clawdlinux/atticdb/blob/main/CONTRIBUTING.md) first.
Short version: open an issue before you code, and keep pull requests small, with tests.
Sign off every commit with `git commit -s`.

We had 0 GitHub stars when I wrote this. If the post helped, a [star](https://github.com/Clawdlinux/atticdb) helps other people find it.
That's the only time I'll ask.

## Where it stands

AtticDB is early. Search matches words, not meaning. Search by meaning is tracked in [#117](https://github.com/Clawdlinux/atticdb/issues/117).
It's good on your laptop. Don't run it in production yet.

What did your agent forget this week?
