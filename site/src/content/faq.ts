export type Qa = { q: string; a: string };

// Home questions a first-time visitor asks. Plain words: no jargon from the onboarding audit's list.
export const HOME_FAQ: Qa[] = [
  {
    q: "What changes in my agent?",
    a: "Each new chat in a repo starts with a short memory block. It holds your rules and that repo's 5 newest notes. This works in Claude Code, Codex, Copilot and VS Code. Your agent can also save and search memory during a chat. To see the exact block, run atticdb studio and open Chat start.",
  },
  {
    q: "Where is my data?",
    a: "On your own disk. On macOS it's in ~/Library/Application Support/atticdb. On Linux it's in ~/.local/share/atticdb. AtticDB has no cloud and no account. Your app does send the memory block to its model, like the rest of your chat.",
  },
  {
    q: "How do I uninstall it?",
    a: "Run atticdb disconnect with each app's name, such as atticdb disconnect claude-code. Then stop the background server and delete the binary and the data folder. The install docs list every command under Uninstall.",
  },
  {
    q: "Is it free?",
    a: "Yes. It's open source under Apache-2.0 or MIT, and it runs on your laptop for free. A paid cloud version is planned. The local version stays free.",
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
    a: "When enough people ask. Join the waitlist from the buttons above and we'll email you when it opens.",
  },
];
