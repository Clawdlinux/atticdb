export type MemoryRecord = {
  kind: "turn" | "fact";
  session?: string;
  key?: string;
  text: string;
  at: string;
};

export const SAMPLE_MEMORY: MemoryRecord[] = [
  {
    kind: "turn",
    session: "atticdb@main",
    text: "Why did we pick ext4 for the durability runs? We need a tested filesystem for fdatasync and directory fsync.",
    at: "2026-10-20T09:15:00.000Z",
  },
  {
    kind: "fact",
    key: "repo/atticdb/fs",
    text: "Run Linux durability tests on ext4. Keep the data directory off NFS and cloud-synced folders.",
    at: "2026-10-21T10:30:00.000Z",
  },
  {
    kind: "turn",
    session: "atticdb@fix/tape-recovery",
    text: "Traced tape recovery through the Rust reader. Add a regression case for a torn tail before changing the replay loop.",
    at: "2026-10-21T15:20:00.000Z",
  },
  {
    kind: "fact",
    key: "user/prefs/editor",
    text: "I use VS Code for Rust. Keep diffs focused and run the nearest test before the full workspace suite.",
    at: "2026-10-22T08:45:00.000Z",
  },
  {
    kind: "turn",
    session: "atticdb@fix/disk-full",
    text: "Checked the disk-full path. Below the free-space floor, writes stop while existing turns and facts remain readable.",
    at: "2026-10-22T14:10:00.000Z",
  },
  {
    kind: "fact",
    key: "repo/atticdb/bench",
    text: "Interleave benchmark systems across 7 reps. Use a fresh data directory for each run and keep raw results.",
    at: "2026-10-23T11:05:00.000Z",
  },
  {
    kind: "turn",
    session: "atticdb@bench/h2h",
    text: "Compared durable appends on ext4 with local NVMe and Premium SSD v2. Keep SQLite's lower closed-loop p50 in the report.",
    at: "2026-10-24T16:40:00.000Z",
  },
  {
    kind: "fact",
    key: "repo/atticdb/deploy-region",
    text: "The deploy region is still undecided for the cloud prototype. Local development does not need a hosted service.",
    at: "2026-10-25T09:25:00.000Z",
  },
  {
    kind: "turn",
    session: "atticdb@fix/flaky-test",
    text: "The flaky test waits a fixed interval for the Unix socket. Replace the sleep with a bounded readiness check.",
    at: "2026-10-25T13:55:00.000Z",
  },
  {
    kind: "turn",
    session: "atticdb@main",
    text: "Reviewed fact replacement semantics. A stale if_lsn must return 409 conflict rather than overwrite the current value.",
    at: "2026-10-26T10:10:00.000Z",
  },
  {
    kind: "fact",
    key: "repo/atticdb/testing",
    text: "When a flaky test fails, keep its seed and exact command. A clean retry alone does not explain the failure.",
    at: "2026-10-27T09:35:00.000Z",
  },
  {
    kind: "turn",
    session: "atticdb@fix/mcp-search",
    text: "Checked memory search across sessions and current facts. All query words must match, with the newest records first.",
    at: "2026-10-27T15:45:00.000Z",
  },
  {
    kind: "turn",
    session: "atticdb@main",
    text: "Reviewed setup rollback for editor configs. Disconnect should remove only AtticDB's entry and leave other MCP servers alone.",
    at: "2026-10-28T11:20:00.000Z",
  },
  {
    kind: "fact",
    key: "repo/atticdb/release",
    text: "Check release downloads against SHA256SUMS. Keep installed MCP entries out of the checkout's target directory.",
    at: "2026-10-29T08:50:00.000Z",
  },
  {
    kind: "turn",
    session: "atticdb@main",
    text: "Revisited the deploy region decision. Wait for a cloud requirement before provisioning anything. The Rust binary still runs locally.",
    at: "2026-10-29T16:05:00.000Z",
  },
  {
    kind: "turn",
    session: "atticdb@docs/windows",
    text: "Kept the Windows durability caveat in the docs. Windows has no directory fsync, so don't claim Linux-equivalent guarantees.",
    at: "2026-10-30T10:00:00.000Z",
  },
];

export function searchMemory(query: string, limit = 3): MemoryRecord[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  return SAMPLE_MEMORY.filter((record) => {
    const text = [record.text, record.key ?? "", record.session ?? ""].join(" ").toLowerCase();
    return words.every((word) => text.includes(word));
  })
    .sort((a, b) => b.at.localeCompare(a.at))
    .slice(0, limit);
}
