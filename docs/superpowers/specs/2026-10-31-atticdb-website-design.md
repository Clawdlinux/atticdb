# atticdb.com website: design

Status: approved for build by the owner on 2026-10-31 ("you should brainstorm those things and decide").
Every call below marked "Our call" was made by the builder, not the owner. Check those first.

## Intent

AtticDB needs a site at atticdb.com that does 3 jobs:

1. Get an open source user from "what is this" to `atticdb setup` in one screen.
2. Show where AtticDB wins against other agent memory products, and where it doesn't, with sources.
3. Take sign-ups for the hosted cloud, which doesn't exist yet, with planned pricing.

It must not look like PlanetScale, Neon, Supabase or any dark-gradient-purple database site.
It also must not look like the agent memory competitors (see "Look" below).

## Audience

- **Local users.** Developers who use Claude Code, Copilot CLI, VS Code or Codex daily.
  They arrive from GitHub, Hacker News or a search like "memory for claude code".
- **Cloud prospects.** People who want the same memory on every machine, or shared with a team.
  Our call: we treat the cloud as a waitlist product with planned prices.

## Goals

- First screen says what AtticDB is and gives the install command. A stranger gets it in 5 seconds.
- 4 pages plus a 404: `/`, `/pricing`, `/compare`, `/benchmarks`.
- SEO: one `h1` per page, unique title and description, canonical URLs, sitemap, robots, JSON-LD, OG image.
- Motion with Framer Motion (`motion` package, `motion/react` import).
- Deployable to Cloudflare Pages as static files. The domain is already on Cloudflare.

## Non-goals

- No docs site. "Docs" links to the GitHub README.
- No accounts, login, checkout or payment form. The cloud doesn't exist yet.
- No blog, no CMS, no analytics, no cookie banner.
- No backend. The waitlist is a GitHub issue link.

## Anti-goals (a failure even if it works)

- Any invented proof: customer logos, quotes, user counts, press.
- Any claim about a competitor we can't source. Where they publish nothing, the cell says "Not published".
- Trashing a competitor. We state facts and say where they're the better pick.
- Hiding where AtticDB loses (SQLite's closed-loop p50, no semantic recall).

## Facts the site uses

Product facts come from the AtticDB repo README at commit `89d0de5`:

- Rust. Turn history per session plus keyed facts. A write is acked only after `fdatasync`.
- Single binary. macOS, Linux, Windows x86_64. Linux builds are static.
- Install: `curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh`, then `atticdb setup`.
- `setup` wires Copilot CLI, VS Code, Claude Code and Codex. `atticdb disconnect <app>` undoes it.
- MCP tools: `memory_append`, `memory_recent`, `fact_set`, `fact_get`, `fact_list`, `memory_search`.
- Search is by words, across every session, newest first. No embeddings, no model calls.
- Secrets in content are replaced with placeholders such as `[secret:github-token]` before write.
- Below a free-space floor it goes read-only and keeps serving reads.
- Licence: Apache-2.0 or MIT. Status: early. Don't run it in production.

Benchmark facts come from `bench/H2H.md` (run 2026-10-24, Azure L8s_v3, ext4, 7 reps, medians).
The site copies the tables exactly. Key figures:

- Premium SSD v2: 1.8x to 2.8x the acks/s of Redis, Valkey and DiceDB in closed loop.
- Agent-shaped load on Premium SSD v2: AtticDB p50 1.3 ms against 2.8 to 3.1 ms.
- Local NVMe, 1 writer: AtticDB p50 94 µs, 9,799 acks/s.
- SQLite has a lower closed-loop p50 at every writer count. The site says so.

Competitor facts come from `site/design/competitors-2026-10-31.md`. Each comes with its source URL, seen 2026-10-31.

## Look

### Palette: Oxide, Bone, Signal (Our call)

Taken from AtticDB's own material: magnetic tape oxide, the paper label on a box in the attic, and the disk activity light that blinks when a write lands.
The owner's reference reel was login-gated and Instagram reported it may be removed, so it could not be used.

| Role | Hex | Use |
|---|---|---|
| `--bg` oxide | `#16110E` | Ground, about 2/3 of the page |
| `--surface` | `#221B17` | One tone step up from the ground |
| `--ink` bone | `#EDE6D8` | Text on oxide. Also the light field ground |
| `--muted` | `#A89C8C` | Secondary notes only, never running text |
| `--accent` signal | `#C8F03C` | The action, and the ack light. Once per screen |
| `--accent-ink` | `#16110E` | Text on signal |
| `--field-1` / `--field-1-ink` | `#EDE6D8` / `#16110E` | Light sections |

Why not the dealt "graphite and orange": Letta ships charcoal with burnt orange and MotherDuck ships orange.
Competitor looks we avoid: Mem0 lavender, Zep cream and plum, Supermemory grey and cobalt, Redis midnight and red, Cognee black and emerald, Letta charcoal and orange.

### Type

- Display: Bebas Neue 400, uppercase, tracking -0.02em (-0.06em above 15vw), leading 0.85.
- Text: EB Garamond 400/500 with italics, leading 1.45. Body 17 to 19px.
- Code: JetBrains Mono, only inside `<pre>` and `<code>`.
- All 3 self-hosted through `next/font/google`. No request to Google at runtime.
- Five size steps from the sheet: small, body, lead, head, display.

### Direction

`site/design/direction-sheet.md` is the art direction, dealt by the taste skill (seed 328461324, "The sentence is the layout").
Follow its blueprints and house rules, with the palette above replacing its section 4.

The idea sentence: **On this page, the fdatasync ack becomes the live word inside the headline.**
Competitor test: Mem0, Zep and Supermemory don't publish an ack-after-sync promise, so the sentence fails for them.

House rules that bind every page (from the taste skill): headlines 2 lines max at every width.
No eyebrows, no monospace outside code, no 01/02/03, no thin rules, no gradients, no shadows at rest, no glass.
No icon cards, no emoji, no styled accent word. Equal outer margins. A heading on every section.
Real tables are exempt from the no-rules rule.

## Pages

### `/` home

In this order. Each maps to a sheet blueprint.

1. **Hero** (blueprint "Controls on the sentence", adapted). Headline `MEMORY THAT` / `HITS THE DISK`.
   Inline on line 1's baseline sits the ack chip: the signal light plus `acked 94 µs`.
   Supporting sentence: "AtticDB is an open-source memory database for coding agents. It acks a write only after fdatasync. One Rust binary, no model calls."
   Under the sentence, the install command itself in a `<code>` block with a copy button. It's on the first screen at every width, phones included (Our call, from the builder check).
   Primary button "Install AtticDB" copies the command. Secondary link "Join the cloud waitlist".
   3 live chips (the live thing, "Pull the plug"):
   - Disk: `Local NVMe` or `Premium SSD v2`.
   - Writers: `1`, `4`, `16`.
   - Power switch.
   Disk and writers set the ack chip to AtticDB's closed-loop p50 from H2H.md for that cell.
   The power switch runs a 2.5 s demo. Writes stream, power cuts, and the store "restarts".
   AtticDB shows every acked write kept. A store that acks before sync shows its unsynced tail lost.
   The demo is labeled "An illustration of ack-after-sync, not a crash test." The H2H bench measures latency, not crash recovery, so the label must not claim more.
   Next to it, one line points at the real test: "The real test cuts power on LazyFS. CI runs crash simulations on 2,000 seeds." It links to `TESTING.md#power-loss` on GitHub.
   The second store is generic, never a named product.
   Without JS, the hero shows the headline, the 94 µs chip, the sentence and the button.
2. **Intro** (blueprint "Pinned sentence, photographs pass"). A statement of 25 words or fewer:
   "Every turn your agent takes is written to a tape on your own disk. Synced, then acked. Nothing leaves the machine."
   3 archival pictures of memory hardware pass beside it: a core memory plane, an IBM 350 RAMAC disk, a reel of magnetic tape.
   A 4th, punched paper tape, sits in the footer or the CTA, so home meets the taste skill's floor of 4 real pictures.
   They come from Wikimedia Commons via the taste skill's `assets.mjs`, with commercial-use licences, credited in the footer.
   Pin for 2 screens at most.
3. **Showcase** (blueprint "Headline you can type"). An input prefilled `why did we pick ext4`.
   Below it, up to 3 results from a sample store, filtered by words, newest first, like `memory_search`.
   Example buttons: `deploy region`, `user prefs`, `flaky test`. Heading: "Search what your agent remembered".
4. **How it works** (blueprint "Stages as one bar"). Heading "Three steps, then forget about it".
   Segments: Install "About ten seconds", Connect your apps "Under a minute", Your agent remembers "Every chat after that".
   Flex-grow 1, 2, 9. Commands shown under each: the curl line, `atticdb setup`, nothing.
5. **Works with** (blueprint "Names as one sentence", as a statement at 36px max).
   "Wires itself into Claude Code, GitHub Copilot CLI, VS Code and Codex. On macOS, Linux and Windows." The lit name cycles every 2 s.
6. **Numbers** (blueprint "Column aligned on the decimal"). Heading "Measured, not promised".
   Rows: `2.8` "times the durable acks of Redis, Valkey and DiceDB, at most. Premium SSD v2" (the range is 1.8 to 2.8; 2.8 is against DiceDB at 4 writers); `1.3` "ms p50 under agent load, Premium SSD v2"; `94` "µs p50, 1 writer, local NVMe"; `0` "model calls per write"; `1` "binary, no other services".
   Link to `/benchmarks`.
7. **Pricing teaser** (blueprint "One plan as a sentence"). "Local is free. Cloud is [$15] a month." The price sits on a signal block.
   Supporting: "The binary is free forever. Cloud adds sync across your machines and backups. Planned pricing." Button "See pricing".
8. **FAQ** (blueprint "Large questions, answers beside"). Heading "Questions". 6 pairs, all open:
   Is it production ready? / Does it call a model? / Where is my data? / How is it different from Mem0 or Zep? / What happens if the disk fills? / What will the cloud add?
   Answers come from the README facts above.
9. **CTA** (blueprint "Question at both edges"). `READY TO` / `REMEMBER?` with "Install AtticDB" (copies the command) or "Star on GitHub".
10. **Footer** (blueprint "Contact as the headline", on the signal field). Display line `ATTICDB`.
    Links: GitHub, Docs, Pricing, Compare, Benchmarks, Issues. Image credits. Licence line.

### `/pricing`

- h1 `FREE ON YOUR DISK.` / `CLOUD WHEN YOU WANT IT.` Fits 2 lines at every width or gets cut.
- 4 plans (Our call):

| Plan | Price | Status | Includes |
|---|---|---|---|
| Local | $0 forever | Available | Everything in the binary: sessions, facts, search, MCP, setup for 4 apps, secret redaction, studio. Apache-2.0 or MIT |
| Cloud Pro | $15 per month | Planned | 1 person, all your machines. Same API and MCP. Synced memory. 10 GB. Daily snapshots, 30-day restore |
| Cloud Team | $29 per seat per month | Planned | Shared team memory. Per-agent tokens. Roles. GitHub and Google sign-in. Audit log. 25 GB per seat, pooled |
| Your cloud | Talk to us | Planned | Runs in your VPC or air-gapped. SAML SSO. Support SLA |

- Line under the plans: "No per-memory or per-call fees. AtticDB makes no model calls when it writes, so we have no tokens to resell. Storage is the meter."
- Line: "Cloud pricing is planned and may change before launch. Local stays free."
- Cloud buttons go to the waitlist: `https://github.com/Clawdlinux/atticdb/issues/new?template=feature.yml&title=Cloud%20waitlist%3A%20`.
  "Your cloud" goes to the same URL with title `Cloud%20in%20my%20VPC%3A%20`.
- A feature table across the 4 plans (a real `<table>`).
- A pricing FAQ: 4 questions. Answers (Our call, planned and labeled so):
  - What counts as storage? "The bytes of your turns, facts and fact history on disk. Forgotten records stop counting after compaction."
  - Can I move from local to cloud? "That's the plan. Same API and MCP tools, so your agents won't notice. The import tool ships with cloud."
  - What if I exceed 10 GB? "Planned: $1 per extra GB per month. Nothing is deleted and writes never stop for billing."
  - When does cloud launch? "When enough people ask. Add a thumbs-up to the waitlist issue."

### `/compare`

- h1 `WHERE WE WIN.` / `WHERE WE DON'T.`
- A real `<table>`: AtticDB, Mem0, Zep and Graphiti, Letta, Supermemory, Redis Agent Memory, Cognee, Engram, CLAUDE.md files.
- Rows: what it stores, storage it needs, model call on write, acked after fsync (published), runs fully offline, licence, MCP, semantic or graph recall.
- Every competitor cell is backed by a source URL in a sources list below the table. Unverified cells say "Not published".
- "Pick AtticDB if" and "Pick something else if" blocks. The second names semantic and graph recall (Mem0, Zep), a full agent harness (Letta), and a mature SQLite tool in Go (Engram).
- Phone: the table scrolls sideways inside its own box, first column sticky.

### `/benchmarks`

- h1 `DURABLE ACKS,` / `MEASURED.`
- Method in plain sentences from H2H.md: the question, setup, the 2 loads, 7 reps, medians.
- An SVG bar chart drawn in code from the data: acks/s by system, 1 group per writer count, per disk. AtticDB in signal, the others in bone tones.
- All 4 tables from H2H.md as real `<table>` elements with the exact numbers.
- "Where we lose": the SQLite sentences from H2H.md.
- Link to the raw TSV files and the runbook on GitHub.

### 404

`NOTHING IN` / `THE ATTIC.` with a link home.

## SEO

- Base URL `https://atticdb.com`. Trailing-slash-free canonical per page.
- Titles: Home "AtticDB: durable memory for coding agents". Pricing "AtticDB pricing: free locally, cloud planned". Compare "AtticDB vs Mem0, Zep, Letta and Supermemory". Benchmarks "AtticDB benchmarks: durable acks vs Redis and SQLite". Each 60 characters or fewer.
- Descriptions 155 characters or fewer, unique per page.
- JSON-LD: `SoftwareApplication` with an `Offer` of price 0 on home. `FAQPage` on home and pricing.
- `sitemap.xml` and `robots.txt` at build. OG image 1200x630, one per site, made from the hero.
- `lang="en"`, semantic landmarks, alt text on every picture.

## Motion

- Signature: the masked headline. Each hero line rises from behind its baseline, 900 ms, strong ease-out, line 2 starts 110 ms later. The sentence and button fade up after.
- Every section: heading, then text, then picture, staggered 60 to 90 ms, rising 16 to 32 px, once on enter.
- Feedback on every control: hover, press and focus.
- One moving picture: a slow scale on the intro's archival photos, or a muted clip if `assets.mjs --video` finds an honest one.
- The ack light pulses once per simulated write.
- Transform and opacity only. `prefers-reduced-motion` keeps content and drops travel (`MotionConfig reducedMotion="user"` plus CSS).
- Content is never hidden without JS. A head script adds `class="js"` to `<html>`. Only `.js` hides reveal targets before they animate.

## Stack (Our call)

- Next.js App Router, TypeScript, `output: "export"`, in `site/` of the atticdb repo.
- Plain CSS with custom properties (one global file plus CSS modules). No Tailwind.
- `motion` for Framer Motion.
- Vitest for the data and logic modules.
- Cloudflare Pages: `wrangler pages deploy site/out --project-name atticdb-site`. The domain is already on Cloudflare, so it's one vendor.
  Vercel would also work from the same static output. We chose Cloudflare.
- A CI job builds and tests `site/` on PRs that touch it.
- 21st.dev MCP is not available in this environment. Components are built from scratch.

## Gate

- `npm test` and `npm run build` pass in `site/`.
- Taste skill: `lint.mjs` clean, `inspect.mjs` 0 FAIL at 1440, 768 and 390 on every page.
  The 4-picture floor binds home. On pricing, compare and benchmarks the pictures are the product's data (tables, chart).
  If the inspector fails those pages only on the picture floor or table rules, pass `--allow` for that rule alone and give the reason in `site/design/critique.md` (Our call).
- Lighthouse-style checks: one h1, every image has alt, contrast 4.5:1 for text.

## Left to the builder

Component boundaries, file names inside `site/`, exact easing curves, exact sample memory records, the chart's geometry.
