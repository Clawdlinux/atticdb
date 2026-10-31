export type Qa = { q: string; a: string };

export const HOME_FAQ: Qa[] = [
  {
    q: "Is it production ready?",
    a: "No. AtticDB is early. Don't run it in production.",
  },
  {
    q: "Does it call a model?",
    a: "No. AtticDB stores turns and facts without model calls. Search finds them by words, without embeddings.",
  },
  {
    q: "Where is my data?",
    a: "On your own disk, in AtticDB's per-user home. ATTICDB_HOME moves it. Don't put it on network filesystems or in folders synced by cloud storage apps.",
  },
  {
    q: "How is it different from Mem0 or Zep?",
    a: "They extract memories with a model call and use a vector or graph store. AtticDB stores turns and facts as written, acks after fdatasync, and finds them by words. See the compare page.",
  },
  {
    q: "What happens if the disk fills?",
    a: "Below a free-space floor, AtticDB goes read-only. It keeps serving reads.",
  },
  {
    q: "What will the cloud add?",
    a: "Planned cloud adds sync across your machines and backups. Cloud Team adds shared team memory. Local stays free.",
  },
];

export const PRICING_FAQ: Qa[] = [
  {
    q: "What counts as storage?",
    a: "The bytes of your turns, facts and fact history on disk. Forgotten records stop counting after compaction.",
  },
  {
    q: "Can I move from local to cloud?",
    a: "That's the plan. Same API and MCP tools, so your agents won't notice. The import tool ships with cloud.",
  },
  {
    q: "What if I exceed 10 GB?",
    a: "Planned: $1 per extra GB per month. Nothing is deleted and writes never stop for billing.",
  },
  {
    q: "When does cloud launch?",
    a: "When enough people ask. Add a thumbs-up to the waitlist issue.",
  },
];
