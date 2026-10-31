# atticdb.com: install, docs and Vercel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an interactive installer, docs served at docs.atticdb.com, and Vercel config to the existing `site/`.

**Architecture:** The installer is a pure step builder plus a client component. Docs are Markdown files rendered at build under `/docs/*`; `vercel.json` host rules serve them on the docs subdomain. Static export stays.

**Tech Stack:** Next.js 16 (output export), React 19, `motion/react`, Vitest, `marked` (new), Vercel static hosting.

**Spec:** `docs/superpowers/specs/2026-10-31-atticdb-website-design.md`, section "Addendum 2026-10-31". Earlier sections still bind.

## Global Constraints

- Everything in the first plan's Global Constraints still holds (`docs/superpowers/plans/2026-10-31-atticdb-website.md`), except the `.js [data-reveal]` line, replaced by: nothing is hidden by CSS before JS runs.
- Every command shown exists in `crates/atticdb/src/main.rs` or `setup.rs`. Harness names: `copilot`, `vscode`, `claude-code`, `codex`. Order: copilot, vscode, claude-code, codex.
- Docs facts come only from `README.md` and `crates/atticdb/README.md`. No invented flags, paths or numbers.
- Motion: easing `cubic-bezier(0.2, 0, 0, 1)`; durations 120 / 240 / 480 ms; no overshoot; stagger 40 ms, total under 200 ms; transform and opacity only; reduced motion drops travel.
- Don't run any script from `/Users/sunny/.agents/skills/design-taste-frontend` (blocked by SkillSpector). Check visuals with headless Chrome screenshots.
- No new dependency except `marked`.
- Commits: `git commit -s`, no `Co-authored-by`.

## Review Focus

1. No app checked: the installer must not print `--harness ` with an empty list. Test in Task 9.
2. Windows + install script: the script option must not appear for Windows, and switching to Windows while "Install script" is chosen falls back to "Release zip". Test in Task 9.
3. Docs host assets: `docs.atticdb.com/_next/...` must not be rewritten into `/docs/_next/...`. Test in Task 11 on the `vercel.json` rules.
4. Docs links in local preview: built under `/docs` they resolve; on Vercel they're host-relative. Test in Task 10 on `docsHref`.
5. Markdown HTML safety: rendered docs come from repo files only, but raw HTML in Markdown is escaped or dropped. Test in Task 10.

---

### Task 9: Interactive installer

**Files:**
- Create: `site/src/content/install.ts`, `site/src/content/install.test.ts`
- Create: `site/src/components/Installer.tsx`, `site/src/components/Installer.module.css`, `site/src/components/Installer.test.tsx`
- Create: `site/src/components/home/InstallSection.tsx`, `InstallSection.module.css`
- Modify: `site/src/app/page.tsx` (section after How it works), `site/src/lib/motion.ts` (add `INSTALL` timing constants if not present), `site/src/components/home/ClosingCta.tsx` (its `id="install"` moves to the new section; the CTA keeps CopyCommand without the id)

**Interfaces:**
- Produces:
  - `type Os = "macos" | "linux" | "windows"`; `type Method = "script" | "source" | "zip"`; `type App = "copilot" | "vscode" | "claude-code" | "codex"`.
  - `type InstallOptions = { os: Os; method: Method; apps: App[]; preview: boolean; hook: boolean }`.
  - `type InstallStep = { id: string; text: string; commands: string[] }` (`commands` empty for a text-only step).
  - `DEFAULT_OPTIONS: InstallOptions` = macos, script, all 4 apps, preview true, hook true.
  - `methodsFor(os: Os): Method[]` → macos/linux `["script","source"]`, windows `["zip","source"]`.
  - `normalize(o: InstallOptions): InstallOptions` → method falls back to `methodsFor(os)[0]` when not allowed; apps deduped and ordered.
  - `buildInstallSteps(o: InstallOptions): InstallStep[]`.
  - `detectOs(ua: string, platform?: string): Os`.
  - `APP_LABELS: Record<App,string>` = Copilot CLI, VS Code, Claude Code, Codex.
  - `Installer({ initial?: Partial<InstallOptions> })` client component.
- Consumes: `INSTALL_CMD`, `GITHUB_URL` from `@/lib/site`; `CopyCommand`; `Reveal`; motion constants.

Step texts and commands (exact):
- script: id `install`, "Install the binary. It checks the download against SHA256SUMS and never uses sudo.", `[INSTALL_CMD]`; then id `path`, "If the installer says ~/.local/bin isn't on your PATH, add it.", `['export PATH="$HOME/.local/bin:$PATH"']`.
- source: id `install`, "Build from source with Rust installed. It lands in ~/.cargo/bin.", `["cargo install --git https://github.com/Clawdlinux/atticdb atticdb --locked"]`.
- zip: id `download`, "Download atticdb-x86_64-pc-windows-msvc.zip and SHA256SUMS from the latest release.", `[]`; id `install`, "In PowerShell, check the zip, unpack it and put it on your PATH.", the 3 lines `(Get-FileHash .\atticdb-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()`, `Expand-Archive .\atticdb-x86_64-pc-windows-msvc.zip "$env:LOCALAPPDATA\Programs\atticdb"`, `$env:PATH += ";$env:LOCALAPPDATA\Programs\atticdb"`. The download step links to `${GITHUB_URL}/releases/latest` in the component.
- apps present and preview: id `preview`, "See what setup would change. It writes nothing.", `["atticdb setup --harness <list> --dry-run" + hookFlag]`.
- apps present: id `setup`, "Connect your apps. Every edited file is backed up first.", `["atticdb setup --harness <list>" + hookFlag]`.
- apps present: id `doctor`, "Check the install. It writes nothing.", `["atticdb doctor --harness <list>"]`.
- no apps: id `pick`, "Pick at least 1 app to connect.", `[]`.
- `<list>` is the checked apps joined by `,` in the order above. `hookFlag` is ` --no-hook` when `hook` is false and the list has `claude-code` or `codex`; otherwise empty.

- [ ] **Step 1: Write failing tests** `install.test.ts`:
  - `buildInstallSteps(DEFAULT_OPTIONS).map(s => s.id)` → `["install","path","preview","setup","doctor"]`; setup command `"atticdb setup --harness copilot,vscode,claude-code,codex"`; doctor `"atticdb doctor --harness copilot,vscode,claude-code,codex"`; preview ends with `--dry-run`.
  - preview false → ids without `preview`.
  - apps `["codex","copilot"]` → list `copilot,codex`.
  - hook false with `["vscode"]` → no `--no-hook`; hook false with `["vscode","codex"]` → setup ends `--no-hook`.
  - apps `[]` → ids `["install","path","pick"]`, and no command anywhere contains `--harness`.
  - `normalize({ ...DEFAULT_OPTIONS, os: "windows" }).method` → `"zip"`; windows zip steps ids start `["download","install"]` and the install step has 3 commands, the first starting `(Get-FileHash`.
  - source on linux → install command starts `cargo install --git`.
  - `detectOs` for a Mac UA → `macos`; a Windows UA → `windows`; an X11 Linux UA → `linux`; Android UA → `linux`; unknown → `macos`.
  - No string in `install.ts` contains `—`.
- [ ] **Step 2:** Run `npm test`. Expected: FAIL (module missing).
- [ ] **Step 3:** Implement `install.ts`.
- [ ] **Step 4: Write failing component tests** `Installer.test.tsx`: renders 3 system options as a radio group with macOS checked by default; 4 app checkboxes all checked; the setup command text is visible; unchecking all 4 shows "Pick at least 1 app to connect."; choosing Windows hides "Install script" and shows "Release zip"; "Copy all" calls `navigator.clipboard.writeText` with every command joined by `\n`; server render (`renderToString`) contains the macOS setup command.
- [ ] **Step 5:** Run `npm test`. Expected: FAIL.
- [ ] **Step 6:** Build `Installer` and `InstallSection` per spec addendum "Interactive install" and "Motion for the installer". Layout: on desktop, controls in a left column (4 of 12) and the steps as a terminal-like panel (`data-ui`, `--surface` on `--bg`, or `--bg` on `--field-1`) in the right 8; on phones, controls stack above steps. Controls are real `<fieldset>`/`<legend>` radio groups and checkboxes styled as solid tiles with 44 px targets. Steps are an `<ol>`. Section heading (h2): "Install it your way". Supporting sentence: "Pick your system and your apps. The commands below are exactly what to run." Detect OS in an effect after mount. Use `motion/react` (`AnimatePresence`, `layoutId`) with the constants. Keep CopyCommand for each command.
- [ ] **Step 7:** Run `npm test`, `npm run lint`, `npm run build`. Screenshot home's install section at 1440 and 390 (headless Chrome, `python3 -m http.server` on `out/`, kill by PID) into `site/.taste-skill/task9/`, look, fix.
- [ ] **Step 8:** Commit `feat(site): interactive installer for local users`.

### Task 10: Docs pages

**Files:**
- Create: `site/content/docs/{index,install,connect,mcp,cli,http-api,how-it-works,windows}.md`
- Create: `site/src/content/docs.ts`, `site/src/content/docs.test.ts`
- Create: `site/src/app/docs/layout.tsx`, `site/src/app/docs/docs.module.css`, `site/src/app/docs/page.tsx`, `site/src/app/docs/[slug]/page.tsx`
- Create: `site/src/app/docs/sitemap.xml/route.ts`, `site/src/app/docs/robots.txt/route.ts`
- Modify: `site/package.json` (add `marked`), `site/next.config.ts` (`env: { DOCS_BASE: process.env.VERCEL ? "" : "/docs" }`), `site/src/lib/site.ts` (`DOCS_URL = "https://docs.atticdb.com"`, `DOCS_ORIGIN`), `site/src/lib/site.test.ts`, `site/src/lib/seo.ts` (docs metadata helper)

**Interfaces:**
- Consumes: `Installer` (Task 9), `GITHUB_URL`.
- Produces:
  - `type DocPage = { slug: "" | "install" | "connect" | "mcp" | "cli" | "http-api" | "how-it-works" | "windows"; title: string; description: string; source: "README.md" | "crates/atticdb/README.md" }`; `DOC_PAGES: DocPage[]` in spec order.
  - `docsHref(slug: string, base = process.env.DOCS_BASE ?? "/docs"): string` → `base + "/" + slug`, with `""` slug giving `base || "/"`.
  - `docCanonical(slug: string): string` → `https://docs.atticdb.com` or `https://docs.atticdb.com/<slug>`.
  - `renderDoc(slug: string): Promise<{ html: string; headings: { id: string; text: string }[] }>` reads `content/docs/<slug or index>.md`, renders with `marked`, escapes raw HTML, adds ids to h2s, rewrites links that start with `docs:` to `docsHref(...)`.
  - `docMetadata(slug): Metadata` with title `"<title> · AtticDB docs"` (60 chars max), description, canonical `docCanonical(slug)`.
- [ ] **Step 1: Write the Markdown.** For each page, copy the facts from the named source sections (README "Where to run it", "Quick start", "MCP", "Secrets", "Connect", "Shell verbs", "Studio", "How it works", "Sandboxed agents"; crates README "Set up", "atticdb doctor", "atticdb serve", "HTTP API", "The outcome rule", "atticdb mcp", "connect and disconnect", "Claude Code", "Codex", "AtticDB memory", "hook session-start", "studio", "Windows"). Shorten sentences only. Don't add facts. Links to other docs pages use `docs:<slug>`. Repo-relative links become absolute GitHub URLs. The install page body says one line, then the Installer renders below it. End each file with `Source: [<file>](<GitHub URL>)`.
- [ ] **Step 2: Write failing tests** `docs.test.ts`: `DOC_PAGES.length === 8`, slugs in order; every slug has a Markdown file; `docsHref("mcp", "/docs") === "/docs/mcp"`, `docsHref("mcp", "") === "/mcp"`, `docsHref("", "") === "/"`, `docsHref("", "/docs") === "/docs"`; `docCanonical("")` and `docCanonical("mcp")`; `renderDoc("mcp")` html contains `memory_append` and an `<h2 id=`; a Markdown string with `<script>alert(1)</script>` renders without a `<script` tag (export the internal `renderMarkdown(md)` for this); `docs:` links are rewritten; no Markdown file contains `—`; every command in a fenced code block on the cli and connect pages starts with `atticdb `, `curl `, `cargo ` or `export ` (guards invented verbs); every titled `docMetadata` title ≤ 60 chars and description ≤ 155.
- [ ] **Step 3:** `npm test`. Expected: FAIL.
- [ ] **Step 4:** Implement `docs.ts`, the layout (sidebar on desktop at 3 of 12 columns with the current page marked by `aria-current="page"` and a tone step; page list above content on phones; prev and next links at the foot), `page.tsx` for overview and `[slug]/page.tsx` with `generateStaticParams` and `dynamicParams = false`. Prose styles: EB Garamond 18–19 px, 68ch measure, h2 in Bebas at `--step-head`... use `--step-lead` for h3; code blocks JetBrains Mono on `--surface`, horizontal scroll inside; tables allowed. The docs sitemap and robots routes are `force-static` and list `docCanonical` URLs and `https://docs.atticdb.com/sitemap.xml`. Point the nav and footer Docs link to `DOCS_URL`.
- [ ] **Step 5:** `npm test`, `npm run lint`, `npm run build`; `out/docs.html`, `out/docs/mcp.html` and `out/docs/sitemap.xml` exist. Screenshot `/docs`, `/docs/install` and `/docs/http-api` at 1440 and 390 into `site/.taste-skill/task10/`, look, fix.
- [ ] **Step 6:** Commit `feat(site): docs pages from the READMEs`.

### Task 11: Vercel config

**Files:**
- Create: `site/vercel.json`, `site/src/lib/vercel.test.ts`
- Delete: `site/public/_headers`, `site/wrangler.toml`
- Modify: `site/README.md` (deploy section for Vercel and Cloudflare DNS), `site/src/lib/seo.test.ts` or others that reference `_headers`

**Interfaces:**
- Consumes: docs routes from Task 10.
- Produces: `vercel.json` with `buildCommand "npm run build"`, `outputDirectory "out"`, `framework null`, `cleanUrls true`, `trailingSlash false`, and:
  - `redirects`: host `www.atticdb.com` → `https://atticdb.com/:path*` (permanent); host `atticdb.com` path `/docs` and `/docs/:path*` → `https://docs.atticdb.com/` and `https://docs.atticdb.com/:path*` (permanent).
  - `rewrites`: host `docs.atticdb.com`: `/` → `/docs`; `/sitemap.xml` → `/docs/sitemap.xml`; `/robots.txt` → `/docs/robots.txt`; `/:path((?!_next/|images/|fonts/|icon\\.svg|og\\.png|docs/).*)` → `/docs/:path`.
  - `headers`: `/(.*)` gets the 3 headers from the old `_headers` with identical values; `/_next/static/(.*)` gets `Cache-Control: public, max-age=31536000, immutable`.
- [ ] **Step 1: Write failing test** `vercel.test.ts` reading `site/vercel.json`: shape above; a small matcher converts each `source` to a RegExp (path-to-regexp style for `:path*` and the explicit group) and asserts on the docs host `/_next/static/x.js`, `/images/a.jpg`, `/icon.svg` are not rewritten and `/mcp` → `/docs/mcp`; the CSP value equals the old string exactly.
- [ ] **Step 2:** `npm test`. Expected: FAIL.
- [ ] **Step 3:** Write `vercel.json`, delete the 2 Cloudflare files, update README (Vercel: `vercel link`, `vercel deploy`, `vercel deploy --prod`, `vercel domains add`; Cloudflare DNS: the 3 records, DNS-only, and why wrangler can't write them).
- [ ] **Step 4:** `npm test`, `npm run lint`, `npm run build`. Expected PASS; `out/_headers` absent.
- [ ] **Step 5:** Commit `feat(site): Vercel config with docs subdomain rules`.
