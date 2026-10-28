"""Eviction policies over a trace, with bounds on the offline optimum.

The model, shared by every policy here and by the brute-force `opt`:
- The cache holds objects of total size at most `budget` after every access.
- An access hits if its object is held. Only the accessed object can enter.
  Any held object can leave at any time, for free.
- A policy may skip admitting an object (bypass). An object larger than the
  budget is never held.

Each function returns (misses, missed tokens) over non-first accesses. First
accesses are compulsory misses for every policy, so they are counted elsewhere.
"""

from __future__ import annotations

import heapq
import math
from collections import Counter, OrderedDict, deque
from functools import lru_cache
from typing import Sequence

from .trace import Access

INF = math.inf


def lru(acc: Sequence[Access], budget: int) -> tuple[int, int]:
    """Token-weighted LRU. A hit makes the object the newest. Evicts the least recent."""
    return _queue(acc, budget, refresh=True)


def fifo(acc: Sequence[Access], budget: int) -> tuple[int, int]:
    """FIFO by insertion: keeps the newest tokens that entered. A hit changes nothing.
    Threshold compaction approximates this."""
    return _queue(acc, budget, refresh=False)


def _queue(acc, budget, refresh):
    held: OrderedDict[str, int] = OrderedDict()  # oldest first
    used = misses = missed = 0
    for a in acc:
        if a.obj in held:
            if refresh:
                held.move_to_end(a.obj)
            continue
        if not a.first:
            misses, missed = misses + 1, missed + a.tokens
        if a.tokens > budget:
            continue
        while used + a.tokens > budget:
            used -= held.popitem(last=False)[1]
        held[a.obj] = a.tokens
        used += a.tokens
    return misses, missed


def gdsf(acc: Sequence[Access], budget: int) -> tuple[int, int]:
    """Greedy-Dual-Size-Frequency (Cherkasova 1998) with cost 1. Priority is
    L + frequency / size. Evicts the lowest priority and raises L to it. The
    frequency counts hits since the object entered."""
    held: dict[str, tuple[float, int, int, int]] = {}  # obj -> (priority, frequency, seq, size)
    heap: list[tuple[float, int, str]] = []
    inflation, used, misses, missed, seq = 0.0, 0, 0, 0, 0
    for a in acc:
        if a.obj in held:
            f = held[a.obj][1] + 1
            seq += 1
            held[a.obj] = (inflation + f / a.tokens, f, seq, a.tokens)
            heapq.heappush(heap, (held[a.obj][0], seq, a.obj))
            continue
        if not a.first:
            misses, missed = misses + 1, missed + a.tokens
        if a.tokens > budget:
            continue
        while used + a.tokens > budget:
            p, s, victim = heapq.heappop(heap)
            if victim not in held or held[victim][2] != s:
                continue  # stale entry
            inflation = p
            used -= held.pop(victim)[3]
        seq += 1
        held[a.obj] = (inflation + 1 / a.tokens, 1, seq, a.tokens)
        heapq.heappush(heap, (held[a.obj][0], seq, a.obj))
        used += a.tokens
    return misses, missed


def keep_last(acc: Sequence[Access], n: float, budget: float = INF) -> tuple[int, int, int, int]:
    """Holds the objects of the newest `n` tool outputs (accesses with ref False).
    References do not refresh. Returns (misses, missed, area, peak): area sums the
    held tokens over the gaps between accesses, peak is the most held at once."""
    window: deque[Access] = deque()
    count: Counter = Counter()
    used = misses = missed = area = peak = 0
    last = len(acc) - 1
    for i, a in enumerate(acc):
        if count[a.obj] == 0 and not a.first:
            misses, missed = misses + 1, missed + a.tokens
        if not a.ref and n > 0:
            window.append(a)
            if a.tokens <= budget:
                used += a.tokens if count[a.obj] == 0 else 0
                count[a.obj] += 1
            if len(window) > n:
                old = window.popleft()
                if old.tokens <= budget:
                    count[old.obj] -= 1
                    used -= old.tokens if count[old.obj] == 0 else 0
        if i < last:
            area += used
            peak = max(peak, used)
    return misses, missed, area, peak


def clearing_keep(acc: Sequence[Access], budget: int) -> int:
    """The largest N whose average held tokens over the gaps fit the budget.
    Held tokens grow with N at every gap, so a binary search finds it."""
    room = budget * max(0, len(acc) - 1)
    lo, hi = 0, sum(1 for a in acc if not a.ref)
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if keep_last(acc, mid, budget)[2] <= room:
            lo = mid
        else:
            hi = mid - 1
    return lo


def clearing(acc: Sequence[Access], budget: int) -> tuple[int, int]:
    """Tool-result clearing: keep only the newest N tool outputs, with N from
    `clearing_keep`. It fits the budget on average, not at every step."""
    return keep_last(acc, clearing_keep(acc, budget), budget)[:2]


def next_use(acc: Sequence[Access]) -> list[float]:
    nxt: list[float] = [INF] * len(acc)
    last: dict[str, int] = {}
    for i in range(len(acc) - 1, -1, -1):
        nxt[i] = last.get(acc[i].obj, INF)
        last[acc[i].obj] = i
    return nxt


def belady_size(acc: Sequence[Access], budget: int) -> tuple[int, int]:
    """Offline. On a miss, admit the object only if it fits after evicting held
    objects whose next use is later than its own. Those go furthest next use first,
    larger first. It is feasible at every step, so it bounds the optimum from above."""
    nxt = next_use(acc)
    held: dict[str, tuple[float, int]] = {}  # obj -> (next use, size)
    used = misses = missed = 0
    for i, a in enumerate(acc):
        if a.obj in held:
            held[a.obj] = (nxt[i], a.tokens)
            continue
        if not a.first:
            misses, missed = misses + 1, missed + a.tokens
        if a.tokens > budget or nxt[i] == INF:
            continue
        later = sorted(((u, s, o) for o, (u, s) in held.items() if u > nxt[i]), reverse=True)
        free = budget - used
        if free + sum(s for _, s, _ in later) < a.tokens:
            continue  # it would push out something needed sooner
        for _, s, o in later:
            if free >= a.tokens:
                break
            del held[o]
            used, free = used - s, free + s
        held[a.obj] = (nxt[i], a.tokens)
        used += a.tokens
    return misses, missed


def pfoo_l(acc: Sequence[Access], budget: int) -> tuple[int, int]:
    """PFOO-L (Berger, Beckmann, Harchol-Balter, SIGMETRICS 2018). The capacity
    holds only on average over the n - 1 gaps between accesses. A hit needs its
    reuse interval held, at a cost of size x gaps. Any feasible schedule spends at
    most budget x (n - 1), so the relaxed optimum bounds the optimum from below.
    Misses: the cheapest intervals first. Tokens: a fractional knapsack, shortest
    intervals first. Intervals of objects over the budget never count."""
    nxt = next_use(acc)
    spans = [(a.tokens, int(nxt[i]) - i) for i, a in enumerate(acc) if nxt[i] != INF and a.tokens <= budget]
    reuse = [a.tokens for a in acc if not a.first]
    room = budget * max(0, len(acc) - 1)

    hits, left = 0, room
    for cost in sorted(s * g for s, g in spans):
        if cost > left:
            break
        hits, left = hits + 1, left - cost

    saved, left = 0, room
    for s, g in sorted(spans, key=lambda x: x[1]):
        if s * g <= left:
            saved, left = saved + s, left - s * g
        else:
            saved += left // g  # the fractional part, rounded down: still a bound
            break
    return len(reuse) - hits, sum(reuse) - saved


def opt(acc: Sequence[Access], budget: int) -> tuple[int, int]:
    """The exact optimum, by search over every hold and evict choice. For tiny traces.
    Returns (fewest misses, fewest missed tokens). Each comes from its own schedule."""
    if len(acc) > 16:
        raise ValueError("opt is exponential; use at most 16 accesses")
    objs = sorted({a.obj for a in acc})
    bit = {o: 1 << k for k, o in enumerate(objs)}
    size = {bit[a.obj]: a.tokens for a in acc}

    def weight(mask):
        return sum(s for b, s in size.items() if mask & b)

    @lru_cache(maxsize=None)
    def best(t: int, held: int, by_tokens: bool) -> int:
        if t == len(acc):
            return 0
        a = acc[t]
        b = bit[a.obj]
        cost = 0 if held & b or a.first else (a.tokens if by_tokens else 1)
        pool, sub, rest = held | b, held | b, INF
        while True:
            if weight(sub) <= budget:
                rest = min(rest, best(t + 1, sub, by_tokens))
            if sub == 0:
                break
            sub = (sub - 1) & pool
        return cost + rest

    return best(0, 0, False), best(0, 0, True)


POLICIES = {
    "lru": lru,
    "fifo": fifo,
    "clearing": clearing,
    "gdsf": gdsf,
    "belady_size": belady_size,
    "pfoo_l": pfoo_l,
}
ONLINE = ("lru", "fifo", "clearing", "gdsf")
