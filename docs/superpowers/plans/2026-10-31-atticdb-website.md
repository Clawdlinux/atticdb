# atticdb.com website Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A static, SEO-ready marketing site for AtticDB in `site/`, with home, pricing, compare, benchmarks and 404 pages, deployable to Cloudflare Pages.

**Architecture:** Next.js App Router with `output: "export"`. Content and numbers live in typed modules under `site/src/content/`, each unit-tested. Pages are server components; interactive parts (hero chips, plug demo, search input, cycling names, reveals) are small client components using `motion/react`.

**Tech Stack:** Next.js 16 (via `create-next-app@16`), React 19, TypeScript, `motion` 14, Vitest 5 + Testing Library + jsdom, plain CSS with custom properties and CSS modules, `next/font/google`.

**Spec:** `docs/superpowers/specs/2026-10-31-atticdb-website-design.md`. Art direction: `site/design/direction-sheet.md`. Competitor sources: `site/design/competitors-2026-10-31.md`. Bench source: `bench/H2H.md`.

## Global Constraints

- All site code lives in `site/`. Run every npm command from `site/`. Never touch Rust code or other folders, except: root `.gitignore`, and a new `.github/workflows/site.yml`.
- Palette tokens, exactly: `--bg #16110E`, `--surface #221B17`, `--ink #EDE6D8`, `--muted #A89C8C`, `--accent #C8F03C`, `--accent-ink #16110E`, `--field-1 #EDE6D8`, `--field-1-ink #16110E`. Components reference tokens only, never raw hex.
- Fonts: Bebas Neue 400 (display, uppercase), EB Garamond 400/500 + italics (text), JetBrains Mono (only inside `pre`/`code`). Loaded with `next/font/google`.
- Type steps: `--step-small clamp(0.8125rem, 0.79rem + 0.1vw, 0.875rem)`, `--step-body clamp(1.0625rem, 1rem + 0.25vw, 1.1875rem)`, `--step-lead clamp(1.25rem, 1.1rem + 0.7vw, 1.625rem)`, `--step-head clamp(2.5rem, 1.2rem + 5vw, 5.5rem)`, plus one `--step-display` sized per hero. No sixth size.
- House rules (taste skill): headlines 2 lines max at 1440, 768 and 390 px. No eyebrow labels, no monospace outside code, no 01/02/03 numbering, no hairline rules (real `<table>` excepted), no gradients, no box-shadow at rest, no backdrop blur, no emoji, no icon cards, no styled accent word. One `h1` per page. A heading on every section. Equal outer margins. One button shape: square corners, solid fill, 44 px min height.
- Content is visible without JS. `<html>` gets class `js` from an inline head script. Only `.js [data-reveal]` starts hidden.
- Motion: transform and opacity only. `MotionConfig reducedMotion="user"` at the root and a `prefers-reduced-motion` CSS block.
- Copy: no em dashes. Sentences of 20 words or fewer. No invented customers, quotes, reviews, counts or press.
- Competitor claims only from `site/design/competitors-2026-10-31.md`, with their URL. Unverified means the cell reads `Not published`.
- Bench numbers copied exactly from `bench/H2H.md`.
- Waitlist URL: `https://github.com/Clawdlinux/atticdb/issues/new?template=feature.yml&title=Cloud%20waitlist%3A%20`. VPC URL: same with `title=Cloud%20in%20my%20VPC%3A%20`.
- Install command: `curl -fsSL https://github.com/Clawdlinux/atticdb/releases/latest/download/install.sh | sh`.
- Site URL `https://atticdb.com`. GitHub `https://github.com/Clawdlinux/atticdb`.
- Commits: `git commit -s`. No `Co-authored-by` trailer. Conventional prefix (`feat(site):`, `test(site):`, `ci(site):`).
- Taste skill path: `/Users/sunny/.agents/skills/design-taste-frontend`. Its state lives in `site/.taste-skill/` (git-ignored).

## Review Focus

1. A 390 px phone: the hero headline plus inline ack chip must stay 2 lines; the chip drops between lines per the sheet. Test: inspect at 390 shows no headline over 2 lines (Task 8).
2. JS disabled: every section's text shows. Test in Task 3: render without the `js` class and assert reveal targets have no inline `opacity: 0`. Task 8 loads `out/index.html` with JS off in Chrome.
3. Reduced motion: the plug demo still runs to its result, without travel. Test in Task 3: the demo reducer reaches `restarted` regardless of motion.
4. Static export paths: `/pricing` must load as `out/pricing.html` on Cloudflare Pages, with no trailing-slash redirect loop. Test in Task 7: the build output contains `pricing.html`, `compare.html`, `benchmarks.html`, `404.html`, `sitemap.xml`, `robots.txt`.
5. Wide tables on phones: the compare and bench tables scroll inside their own box, and the page body never scrolls sideways. Test in Task 8: inspect reports no overflow at 390.

---

### Task 1: Scaffold, tokens, layout shell, CI

**Files:**
- Create: `site/` via `npx create-next-app@16 site --ts --app --src-dir --eslint --no-tailwind --import-alias "@/*" --use-npm --turbopack` (accept defaults otherwise)
- Modify: `site/next.config.ts` (`output: "export"`, `images.unoptimized: true`, `trailingSlash: false`)
- Create: `site/src/app/globals.css` (replace), `site/src/app/layout.tsx` (replace), `site/src/app/page.tsx` (placeholder h1 only)
- Create: `site/src/components/SiteNav.tsx`, `site/src/components/SiteNav.module.css`, `site/src/components/Providers.tsx`
- Create: `site/src/lib/site.ts`, `site/src/lib/site.test.ts`, `site/vitest.config.ts`
- Create: `.github/workflows/site.yml`
- Modify: root `.gitignore` (add `site/node_modules`, `site/.next`, `site/out`, `site/.taste-skill`, `.superpowers/`)

**Interfaces:**
- Produces: `SITE_URL = "https://atticdb.com"`, `GITHUB_URL`, `DOCS_URL = GITHUB_URL + "#readme"`, `INSTALL_CMD`, `WAITLIST_URL`, `VPC_URL` exported from `@/lib/site`. `absoluteUrl(path: string): string` (no trailing slash except root `https://atticdb.com/`). Root layout exports `metadataBase = new URL(SITE_URL)`. CSS custom properties from Global Constraints in `globals.css`. Font CSS variables `--font-display`, `--font-text`, `--font-code`. `Providers` (client) wraps children in `MotionConfig reducedMotion="user"`. `SiteNav`: wordmark "AtticDB" left; links Pricing, Compare, Benchmarks, Docs; one button "Install" linking to `/#install`.
- Scripts in `package.json`: `dev`, `build`, `test` (`vitest run`), `lint`.

- [ ] **Step 1:** Scaffold with create-next-app, install `motion`, and dev deps `vitest @vitejs/plugin-react jsdom @testing-library/react @testing-library/jest-dom`.
- [ ] **Step 2: Write the failing test** `site/src/lib/site.test.ts`:
  - `absoluteUrl("/")` → `"https://atticdb.com/"`; `absoluteUrl("/pricing")` → `"https://atticdb.com/pricing"`; `absoluteUrl("pricing/")` → `"https://atticdb.com/pricing"`.
  - `INSTALL_CMD` equals the Global Constraints string. `WAITLIST_URL` and `VPC_URL` equal theirs.
- [ ] **Step 3:** Run `npm test`. Expected: FAIL, module missing.
- [ ] **Step 4:** Implement `site.ts`, layout (`lang="en"`, inline `document.documentElement.classList.add('js')` script in head, fonts, `Providers`, `SiteNav`, skip link to `#main`), tokens and reset in `globals.css` including focus-visible ring in `--accent`, selection colours, the reduced-motion block, and `.js [data-reveal]{opacity:0}`.
- [ ] **Step 5:** Run `npm test` and `npm run build`. Expected: PASS, and `site/out/index.html` exists.
- [ ] **Step 6:** Write `.github/workflows/site.yml`: on `pull_request` and `push` to `main` with `paths: ["site/**", ".github/workflows/site.yml"]`; ubuntu-latest, Node 22, `npm ci`, `npm test`, `npm run build`, working-directory `site`.
- [ ] **Step 7:** Commit `feat(site): scaffold Next.js static site with tokens and CI`.

### Task 2: Content modules

**Files:**
- Create: `site/src/content/bench.ts`, `site/src/content/pricing.ts`, `site/src/content/compare.ts`, `site/src/content/faq.ts`, `site/src/content/memory.ts`
- Test: `site/src/content/content.test.ts`

**Interfaces:**
- Consumes: `WAITLIST_URL`, `VPC_URL`, `GITHUB_URL` from `@/lib/site`.
- Produces:
  - `type Disk = "nvme" | "premium-v2"`; `type System = "atticdb" | "redis" | "valkey" | "dicedb" | "sqlite"`; `type BenchRow = { disk: Disk; load: string; writers: number; thinkMs: number; system: System; acksPerSec: number; p50us: number; p99us: number; p99RangeUs: [number, number]; p999us: number; maxUs: number }`; `BENCH_ROWS: BenchRow[]` with every row of the 4 tables in `bench/H2H.md` (closed loop: `load: "closed"`, `thinkMs: 0`; agent-shaped: `load: "agent"`, writers 64 think 5 and writers 256 think 50). `ackFor(disk: Disk, writers: 1 | 4 | 16): { p50us: number; acksPerSec: number }` returns AtticDB closed-loop values. `BENCH_META = { date: "2026-10-24", machine: "Azure Standard_L8s_v3", reps: 7 }`.
  - `type Plan = { id: "local" | "pro" | "team" | "vpc"; name: string; price: string; unit: string; status: "Available" | "Planned"; includes: string[]; cta: { label: string; href: string } }`; `PLANS: Plan[]` per the spec table. `PLAN_FEATURES: { feature: string; values: Record<Plan["id"], string> }[]` for the pricing table. `PRICING_NOTES: string[]` (the 2 spec lines).
  - `type Product = { id: string; name: string; url: string }`; `PRODUCTS` for the 9 columns in the spec order. `type Cell = { value: string; source?: string }`; `COMPARE_ROWS: { label: string; cells: Record<string, Cell> }[]` with the 8 spec rows. AtticDB cells have no source. Every other cell either has a `source` URL from the competitors file or has value `"Not published"`. `PICK_ATTIC: string[]`, `PICK_OTHER: { when: string; product: string }[]`.
  - `type Qa = { q: string; a: string }`; `HOME_FAQ: Qa[]` (6, spec order), `PRICING_FAQ: Qa[]` (4, answers verbatim from the spec's pricing FAQ).
  - `type MemoryRecord = { kind: "turn" | "fact"; session?: string; key?: string; text: string; at: string }` (ISO time); `SAMPLE_MEMORY: MemoryRecord[]` (12 to 20 believable records from a coding agent's week on the atticdb repo); `searchMemory(query: string, limit = 3): MemoryRecord[]`. Split query on whitespace, lowercase, keep records whose lowercased text, key or session contains every word, sort newest first, slice to limit. Empty query returns the newest `limit`.
- [ ] **Step 1: Write the failing tests** in `content.test.ts`:
  - `ackFor("nvme", 1)` → `{ p50us: 94, acksPerSec: 9799 }`; `ackFor("premium-v2", 1)` → `{ p50us: 817, acksPerSec: 1167 }`; `ackFor("premium-v2", 16).acksPerSec` → `9328`.
  - `BENCH_ROWS.length` → `50` (2 disks × (15 closed + 10 agent)).
  - Premium v2 closed 4 writers: atticdb/dicedb acks ratio rounds to `2.8` with 1 decimal; 16 writers atticdb/redis rounds to `1.8`.
  - `PLANS.map(p => p.price)` → `["$0", "$15", "$29", "Talk to us"]`; local status `"Available"`, the rest `"Planned"`; pro and team `cta.href === WAITLIST_URL`; vpc `cta.href === VPC_URL`.
  - Every `COMPARE_ROWS` cell for a non-AtticDB product has `source` starting `https://` or value `"Not published"`. Every row has a cell for every product id.
  - `HOME_FAQ.length === 6`, `PRICING_FAQ.length === 4`. No string in any content module contains `—`.
  - `searchMemory("ext4")` returns at least 1 record, all containing "ext4". `searchMemory("DEPLOY region")` matches case-insensitively. `searchMemory("zzzz")` → `[]`. Results are sorted by `at` descending.
- [ ] **Step 2:** Run `npm test`. Expected: FAIL.
- [ ] **Step 3:** Implement the 5 modules.
- [ ] **Step 4:** Run `npm test`. Expected: PASS.
- [ ] **Step 5:** Commit `feat(site): typed content for bench, pricing, compare, faq and sample memory`.

### Task 3: Home hero with the live ack and "Pull the plug"

**Files:**
- Create: `site/src/components/home/Hero.tsx`, `Hero.module.css`, `AckChip.tsx`, `PlugDemo.tsx`, `plug.ts`, `plug.test.ts`
- Create: `site/src/components/Reveal.tsx`, `site/src/components/Reveal.test.tsx`
- Create: `site/src/components/CopyCommand.tsx`, `CopyCommand.module.css` (client; copies `INSTALL_CMD` with `navigator.clipboard.writeText`, shows "Copied" for 2 s; without JS the `<code>` text is still selectable)
- Modify: `site/src/app/page.tsx`

**Interfaces:**
- Consumes: `ackFor`, `INSTALL_CMD`, `WAITLIST_URL`.
- Produces: `Reveal` client component `({ children, delay?: number, as?: keyof JSX.IntrinsicElements }) => JSX.Element` that renders `data-reveal` and animates opacity 0→1 and y 24→0 once in view (motion `whileInView`, `viewport={{ once: true, amount: 0.3 }}`), with no inline hidden style in server HTML. Later tasks use it for every section entrance.
- `plug.ts`: `type PlugState = { phase: "idle" | "writing" | "cut" | "restarted"; sent: number; acked: number; synced: number; keptAttic: number; keptAckBeforeSync: number }`; `initialPlug(): PlugState`; `stepPlug(s: PlugState, ev: "tick" | "cut" | "restart" | "reset"): PlugState`. Rules: on `tick` while writing, `sent += 1`; AtticDB acks only synced writes (`acked = synced`) and `synced` lags `sent` by 1; the ack-before-sync store acks every sent write but its last 3 are unsynced. On `cut` phase becomes `cut`. On `restart` phase becomes `restarted`, `keptAttic = acked`, `keptAckBeforeSync = max(0, sent - 3)`. `reset` returns `initialPlug()`.

- [ ] **Step 1: Write failing tests** `plug.test.ts`: after `idle`→start writing and 10 ticks, `sent === 10`, `acked === 9`; after `cut` then `restart`, `phase === "restarted"`, `keptAttic === 9` (every acked write kept), `keptAckBeforeSync === 7` while that store had acked 10 (3 acked writes lost); `reset` equals `initialPlug()`. `Reveal.test.tsx`: renders children text; server render (`renderToString`) contains no `opacity:0` in a style attribute.
- [ ] **Step 2:** Run `npm test`. Expected: FAIL.
- [ ] **Step 3:** Implement `plug.ts` and `Reveal`.
- [ ] **Step 4:** Build the hero per spec "Home 1" and sheet section 6. Rules: `h1` text reads "Memory that hits the disk" to screen readers; the visible lines are `MEMORY THAT` and `HITS THE DISK`, each wrapped for the masked reveal (900 ms, ease `[0.16, 1, 0.3, 1]`, line 2 at +110 ms). `AckChip` sits inline on line 1's baseline, 1 line-height tall, em-sized, `data-ui`, showing the signal light and `acked {p50} µs` (format ≥1000 µs as `x.x ms`). 3 chips (`data-ui`, tilted 2 to 4 degrees on desktop, untilted on phone): Disk, Writers, Power. Disk and writers call `ackFor`. Power runs `PlugDemo`: ticks every 120 ms for about 1.5 s, cut, restart, then shows 2 lines: "AtticDB: 9 of 9 acked writes kept." and "A store that acks before sync: 7 of 10 acked writes kept." with the label "An illustration of ack-after-sync, not a crash test." and the line "The real test cuts power on LazyFS. CI runs crash simulations on 2,000 seeds." linking to `https://github.com/Clawdlinux/atticdb/blob/main/TESTING.md#power-loss`. Under the supporting sentence, `CopyCommand` shows `INSTALL_CMD` in a `<code>` block on the first screen at 1440 and 390 (it may wrap or scroll inside its box). Button "Install AtticDB" copies the command; link "Join the cloud waitlist" → `WAITLIST_URL`. Phone layout per sheet ("On a phone").
- [ ] **Step 5:** Run `npm test` and `npm run build`. Expected: PASS. Open `out/index.html` in Chrome with JS disabled: headline, chip, sentence and button visible.
- [ ] **Step 6:** Commit `feat(site): hero with live ack chip and pull-the-plug demo`.

### Task 4: Home sections 2 to 10 and archival pictures

**Files:**
- Create under `site/src/components/home/`: `Intro.tsx`, `Showcase.tsx`, `HowItWorks.tsx`, `WorksWith.tsx`, `Numbers.tsx`, `PricingTeaser.tsx`, `Faq.tsx`, `ClosingCta.tsx`, each with a `.module.css`
- Create: `site/src/components/SiteFooter.tsx`, `SiteFooter.module.css`, `site/src/content/credits.ts`
- Create: `site/public/images/` (downloaded archival pictures, webp or jpg, ≤ 250 KB each)
- Test: `site/src/components/home/home.test.tsx`
- Modify: `site/src/app/page.tsx`, `site/src/app/layout.tsx` (footer)

**Interfaces:**
- Consumes: `Reveal`, `CopyCommand` (Task 3), `searchMemory`, `SAMPLE_MEMORY`, `HOME_FAQ`, `BENCH_ROWS`, `INSTALL_CMD`, `GITHUB_URL`, `DOCS_URL`, `WAITLIST_URL`.
- Produces: `CREDITS: { title: string; creator: string; licence: string; source: string; file: string }[]`; `SiteFooter` used by every page; the `id="install"` anchor lives on the closing CTA's `CopyCommand`. Home carries at least 4 real pictures (3 in Intro, punched paper tape in the closing CTA or footer).

- [ ] **Step 1:** Find pictures: from `site/`, run `node /Users/sunny/.agents/skills/design-taste-frontend/scripts/assets.mjs "magnetic core memory plane"`, then `"IBM 350 RAMAC disk"`, then `"magnetic tape reel"`, then `"punched paper tape"`. Look at candidates, `--get <ref>` 3 or 4 good ones (commercial-use licence only), then `--credits`. Try `--video "hard disk platter"`; use a clip only if it's honest and under 4 MB. Put files in `public/images/` and records in `credits.ts`.
- [ ] **Step 2: Write failing tests** `home.test.tsx`: the home page renders exactly 1 `h1`; every `section` has an `h2`; `Showcase` typing "ext4" shows ≤3 results each containing "ext4"; clicking example `flaky test` refills the input; `Faq` renders 6 questions with answers visible (no `details`, no hidden attr); every `img` has non-empty `alt`; `SiteFooter` lists every `CREDITS` title.
- [ ] **Step 3:** Run `npm test`. Expected: FAIL.
- [ ] **Step 4:** Build sections 2 to 10 per the spec "Home" list and the matching sheet blueprints (geometry from the sheet, copy from the spec, palette from Global Constraints). Fields: Intro on `--bg`; Showcase on `--field-1`; HowItWorks on `--bg` with segments in `--surface`, `--ink`, `--accent`; WorksWith on `--bg`; Numbers on `--field-1`; PricingTeaser on `--bg` with the price on an `--accent` block; Faq on `--field-1`; ClosingCta on `--bg`; footer on `--accent` with `--accent-ink`. Neighbouring sections never share a layout. Intro pin is 2 screens max; the sentence never sits under a picture (pictures pass beside, not over, the words). Every section entrance uses `Reveal` with 60 to 90 ms stagger.
- [ ] **Step 5:** Run `npm test` and `npm run build`. Expected: PASS.
- [ ] **Step 6:** Commit `feat(site): home sections, archival pictures and footer`.

### Task 5: Pricing page

**Files:**
- Create: `site/src/app/pricing/page.tsx`, `site/src/app/pricing/pricing.module.css`
- Test: `site/src/app/pricing/pricing.test.tsx`

**Interfaces:**
- Consumes: `PLANS`, `PLAN_FEATURES`, `PRICING_NOTES`, `PRICING_FAQ`, `Reveal`, `CopyCommand`, `SiteFooter` (via layout).
- Produces: `export const metadata` for the page (title and description per spec).

- [ ] **Step 1: Write failing tests**: 1 `h1` whose text includes "Free on your disk"; 4 plan headings in order Local, Cloud Pro, Cloud Team, Your cloud; each Planned plan shows the word "Planned"; Pro and Team links have `href === WAITLIST_URL`; a `table` with 4 plan column headers and one row per `PLAN_FEATURES`; both `PRICING_NOTES` present; 4 FAQ questions visible.
- [ ] **Step 2:** Run `npm test`. Expected: FAIL.
- [ ] **Step 3:** Build the page. Composition: h1 at display size on `--bg` (2 lines); plans as one 4-column band where Local sits on `--field-1` and the cloud plans on `--surface`, separated by tone steps, never outlines or shadows; the price set at `--step-head` in Bebas; the Local plan's action is `CopyCommand`; then the notes as one statement; then the feature table on `--field-1` (lines allowed, it's a real table; phone: horizontal scroll inside a wrapper with `tabindex="0"` and an accessible label); then FAQ in the home FAQ's two-column layout.
- [ ] **Step 4:** Run `npm test` and `npm run build`. Expected: PASS and `out/pricing.html` exists.
- [ ] **Step 5:** Commit `feat(site): pricing page with planned cloud tiers`.

### Task 6: Compare and benchmarks pages

**Files:**
- Create: `site/src/app/compare/page.tsx`, `compare.module.css`, `site/src/app/benchmarks/page.tsx`, `benchmarks.module.css`
- Create: `site/src/components/BenchChart.tsx` (server component, pure SVG), `site/src/components/DataTable.module.css`
- Test: `site/src/app/compare/compare.test.tsx`, `site/src/app/benchmarks/benchmarks.test.tsx`

**Interfaces:**
- Consumes: `PRODUCTS`, `COMPARE_ROWS`, `PICK_ATTIC`, `PICK_OTHER`, `BENCH_ROWS`, `BENCH_META`, `GITHUB_URL`, `Reveal`.
- Produces: `BenchChart({ disk: Disk })` rendering grouped bars of closed-loop acks/s for writers 1, 4, 16, AtticDB bar in `--accent`, others in `--ink` at reduced opacity steps, with `role="img"` and an `aria-label` summarising the figures, plus a visually hidden table fallback.

- [ ] **Step 1: Write failing tests**: compare has 1 `h1`; a `table` with 9 product column headers and 8 rows; every non-AtticDB cell with a source renders a link to it in the sources list; the text "Not published" appears; a "Pick something else if" heading exists with the 3 cases. Benchmarks has 1 `h1`; 4 tables; the NVMe 1-writer AtticDB row shows `9,799` and `94`; the text includes "SQLite has a lower p50"; links to `bench/h2h-nvme.tsv` and `bench/h2h.sh` on GitHub; 2 `BenchChart` SVGs with `role="img"`.
- [ ] **Step 2:** Run `npm test`. Expected: FAIL.
- [ ] **Step 3:** Build both pages per spec. Numbers formatted with `Intl.NumberFormat("en-US")` and `font-variant-numeric: tabular-nums`. Tables: first column sticky on phone, scroll wrapper with `tabindex="0"`, `role="region"` and `aria-label`.
- [ ] **Step 4:** Run `npm test` and `npm run build`. Expected: PASS; `out/compare.html` and `out/benchmarks.html` exist.
- [ ] **Step 5:** Commit `feat(site): compare and benchmarks pages`.

### Task 7: SEO, 404, OG image, Cloudflare config

**Files:**
- Create: `site/src/app/sitemap.ts`, `site/src/app/robots.ts`, `site/src/app/not-found.tsx`, `site/src/components/JsonLd.tsx`, `site/src/lib/seo.ts`, `site/src/lib/seo.test.ts`
- Create: `site/public/og.png` (1200×630), `site/scripts/og.html`, `site/scripts/make-og.sh`
- Create: `site/public/_headers`, `site/wrangler.toml`, `site/README.md`
- Modify: every `page.tsx` (metadata + JSON-LD), `site/src/app/layout.tsx` (default OG, twitter card)

**Interfaces:**
- Produces: `PAGES: { path: "/" | "/pricing" | "/compare" | "/benchmarks"; title: string; description: string }[]` and `pageMetadata(path): Metadata` (title, description, `alternates.canonical`, `openGraph` with `/og.png`, `twitter.card = "summary_large_image"`). `softwareAppJsonLd()`, `faqJsonLd(items: Qa[])`.

- [ ] **Step 1: Write failing tests** `seo.test.ts`: 4 `PAGES`, titles unique and ≤60 chars, descriptions unique and ≤155 chars; titles equal the spec's exact strings; `pageMetadata("/pricing").alternates.canonical === "https://atticdb.com/pricing"`; `softwareAppJsonLd()` has `@type` `SoftwareApplication`, `offers.price === "0"`, `license` mentions Apache-2.0; `faqJsonLd(HOME_FAQ).mainEntity.length === 6`.
- [ ] **Step 2:** Run `npm test`. Expected: FAIL.
- [ ] **Step 3:** Implement. `sitemap.ts` and `robots.ts` use `export const dynamic = "force-static"`. Robots allows all and points to `https://atticdb.com/sitemap.xml`. `not-found.tsx` per spec. OG image: `og.html` draws the hero (oxide ground, `MEMORY THAT` / `HITS THE DISK`, the signal chip) with local fonts; `make-og.sh` screenshots it with headless Chrome at 1200×630 into `public/og.png`. `_headers`: long cache for `/_next/static/*`, and `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`, a CSP allowing `'self'` plus `'unsafe-inline'` for scripts and styles only. `wrangler.toml`: `name = "atticdb-site"`, `pages_build_output_dir = "out"`. README: dev, test, build, deploy (`npx wrangler login`, `npx wrangler pages deploy out --project-name atticdb-site`, then add `atticdb.com` as a custom domain in the Pages project), and how to refresh competitor facts.
- [ ] **Step 4:** Run `npm test` and `npm run build`. Expected: PASS; `out/` contains `index.html`, `pricing.html`, `compare.html`, `benchmarks.html`, `404.html`, `sitemap.xml`, `robots.txt`, `og.png`, `_headers`. Each HTML has 1 `<link rel="canonical">` and the home has 2 `application/ld+json` scripts.
- [ ] **Step 5:** Commit `feat(site): SEO metadata, sitemap, 404, OG image and Cloudflare config`.

### Task 8: Taste gate, motion pass, fixes

**Files:**
- Modify: any file under `site/src/` the gate flags
- Create: `site/design/critique.md`

**Interfaces:**
- Consumes: everything above. Produces no new interfaces.

- [ ] **Step 1:** From `site/`: `npm run build`, then `npx serve out -l 4321 &` (or `python3 -m http.server 4321 -d out`). Record the PID and kill it at the end.
- [ ] **Step 2:** Run `node /Users/sunny/.agents/skills/design-taste-frontend/scripts/lint.mjs src` and `node /Users/sunny/.agents/skills/design-taste-frontend/scripts/inspect.mjs http://localhost:4321/` then `/pricing`, `/compare`, `/benchmarks` (each with `--out .taste-skill/inspect-<page>`). Expected first time: some FAIL lines.
- [ ] **Step 3:** Fix every FAIL. Treat every WARN as wrong unless one sentence defends it in `critique.md`. Moving-element count must be ≥ 8 on home (≥ 16 preferred). Home must meet the 4-picture floor. On pricing, compare and benchmarks only, `--allow` is permitted for the picture-floor rule and for rules on real `<table>` elements, each with its reason in `critique.md`.
- [ ] **Step 4:** Open every screenshot the inspector lists. Critique per `/Users/sunny/.agents/skills/design-taste-frontend/references/critique.md` §4 format into `site/design/critique.md`. Fix the top 3 problems. Re-run inspect. At most 3 rounds.
- [ ] **Step 5:** Verify JS-off: headless Chrome with `--blink-settings=scriptEnabled=false` screenshot of `/` shows all section text. Verify `prefers-reduced-motion` via inspect's reduced-motion output if present.
- [ ] **Step 6:** Run `npm test`, `npm run lint`, `npm run build`. Expected: all PASS, inspect 0 FAIL on all 4 pages.
- [ ] **Step 7:** Commit `fix(site): pass taste gate at 1440, 768 and 390`.
