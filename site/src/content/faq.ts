export type Qa = { q: string; a: string };

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
    a: "When enough people ask. Join the waitlist from the buttons above and we'll email you when it opens.",
  },
];
