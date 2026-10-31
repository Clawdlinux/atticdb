# Critique: atticdb.com taste gate

Format from the taste skill, `references/critique.md` §4. Screenshots live in `site/.taste-skill/inspect-<page>/`.
Round 1 judged the first build of this task (motion moved to the inline script, sizes on the steps).
Round 2 judged the final build, after the top 3 fixes.

## Allowances

- `--allow few-pictures` on `/pricing` and `/compare`. Their pictures are the product's data: the plan cards and the plan table on pricing, the 9-product matrix on compare. The spec's Gate section binds the 4-picture floor to home only.
- No other `--allow` is used. Benchmarks passes the picture floor through its chart and tables, so it needs none.

## Defended warnings

One sentence each. Every other WARN was fixed.

- `palette-cream` (every page): `#EDE6D8` is the spec's bone, the paper label on the box, a low-chroma off-white fixed by the global constraints, not a yellow cream.
- `font-count` (home, pricing): the third face is JetBrains Mono, set only on `pre` and `code` for the install command, which the house rules allow.
- `no-footage` (every page): no honest clip of the subject exists in the asset search, so the spec's moving picture is the slow scale on the archival photographs instead.
- `no-real-pictures` (benchmarks): the page is a measurement report, so its pictures are the chart and the 4 data tables, as the spec's Gate section says.
- `off-centre` at 1440 (pricing): the right edge reads 120px because the plan table's text columns are now left-aligned as asked, while the nav, the plan cards and the table rule all reach the 80px margin on both sides.
- `off-centre` at 390 (benchmarks): the 4px right edge is table cells and chart labels inside the sideways-scrolling regions of real `<table>` and chart elements, and every block outside them sits on the 16px margins.
- `still-page` at 1440 (404): one screen with one job moves 8 things (2 masked lines, the sentence, the button and 4 drifting photographs), and more would be decoration.

## Home

```
ROUND 1
Seen in one second: the headline MEMORY THAT / HITS THE DISK with the ack chip sitting in it.
Idea (one sentence): the sentence is the layout; the live ack latency sits in the sentence where a word would be.
Live thing: the disk and writer chips change the acked latency; Pull the plug runs the power-cut demo.
Layout words by section: hero=sentence / intro=pinned split / showcase=headline you type / how=one bar / works=statement / numbers=ledger / pricing=sentence / faq=two-column list / cta=question at both edges
Scores: design 7.5 · usability 8 · creativity 8 · content 8 → overall 7.8
Caps applied: none (0 FAIL, 4 real photographs, 49 moving elements).
Top three problems (most visible first), each with the screenshot it shows in and the fix:
1. The Works With statement is a 26px caps line alone in a 250px dark band, so it reads as a caption with a hole beside it (desktop-page-02, mobile-page-01). Fix: the heading gets the heading step; the names follow at the lead step beside it on desktop.
2. On /pricing "TALK TO US" breaks to TALK TO / US in the 4-card row at the heading step, so the fourth card is taller and its terms sit lower (pricing desktop-fold). Fix: plans run 2 by 2 from 768.
3. The numbers ledger at the display step stacks 5 rows with 1.25rem gaps; the fold cuts the 94 in half and the heading floats mid-band (desktop-page-02). Fix: tighten the ledger's row gap on desktop.

ROUND 2
Seen in one second: the same headline and chip; the hero is untouched.
Idea (one sentence): unchanged.
Live thing: unchanged; the sections below now rise in from the inline script even when a JS chunk fails.
Layout words by section: hero=sentence / intro=pinned split / showcase=headline you type / how=one bar / works=split statement / numbers=ledger / pricing=sentence / faq=two-column list / cta=question at both edges
Scores: design 7.8 · usability 8 · creativity 8 · content 8 → overall 7.9
Caps applied: none.
Top three problems (most visible first), each with the screenshot it shows in and the fix:
1. How It Works hangs its 3 clauses in a staircase of small text under the bar; at 1440 the third clause floats far right of the first (desktop-page-02). Left for the next pass: it is the dealt blueprint.
2. The numbers ledger still runs past one screen at 1440, so 94 straddles the fold (desktop-page-02). A smaller figure step would cost the section its poster.
3. The closing CTA leaves a tall dark band between READY TO and the tape strip on the phone (mobile-page-02). It is the "question at both edges" composition, kept as dealt.
```

## Pricing

```
ROUND 1
Seen in one second: FREE ON YOUR DISK. CLOUD WHEN YOU WANT IT.
Idea (one sentence): the price is a sentence; local is free and the cloud is planned.
Live thing: the Local card's copy button copies the install command.
Layout words by section: hero=sentence / plans=4 cards / notes=split / comparison=table / faq=two-column list
Scores: design 7 · usability 8 · creativity 6.5 · content 8 → overall 7.3
Caps applied: none.
Top three problems (most visible first), each with the screenshot it shows in and the fix:
1. "TALK TO US" breaks with an orphan US and pushes the fourth card's terms down (desktop-fold). Fix: 2 by 2 plans.
2. The notes heading was a muted serif eyebrow that repeated the note. Fixed before the round: a real h2, "No per-call fees".
3. The status read as a 13px tag. Fixed before the round: one body-size sentence, "$15 per month. Planned."

ROUND 2
Seen in one second: the 2-line headline, then two plan cards side by side.
Idea (one sentence): unchanged.
Live thing: unchanged.
Layout words by section: hero=sentence / plans=2 by 2 / notes=split / comparison=table / faq=two-column list
Scores: design 7.5 · usability 8.5 · creativity 6.5 · content 8 → overall 7.7
Caps applied: none (few-pictures allowed, see Allowances).
Top three problems (most visible first), each with the screenshot it shows in and the fix:
1. The Local card's install command wraps in 4 short lines at 390 (mobile-page-01). It is the full command and must stay copyable.
2. The plan table's left-aligned last column ends short of the margin (desktop-page-01), defended above.
3. The hero headline drops to 44px at 390, small next to home's 82px; it is sized to fit CLOUD WHEN YOU WANT IT. on one line down to 320.
```

## Compare

```
ROUND 1
Seen in one second: WHERE WE WIN. WHERE WE DON'T.
Idea (one sentence): an honest matrix, every cell sourced or marked Not published.
Live thing: the source numbers jump to their URLs.
Layout words by section: hero=sentence / matrix=table / pick=split list / pick other=split on surface / sources=split list
Scores: design 7 · usability 8 · creativity 6.5 · content 8.5 → overall 7.4
Caps applied: none (few-pictures allowed).
Top three problems (most visible first), each with the screenshot it shows in and the fix:
1. Source marks sat on the baseline at body size, reading as stray numbers in the cells (desktop-page-01). Fix: real superscripts, line-height 0 so rows keep their height.
2. The matrix scrolls sideways at 768 and 390 (tablet-page-01, mobile-page-01). It is a real 10-column table; the first column stays pinned.
3. Three split sections in a row (pick, pick other, sources) repeat one layout word; the surface band breaks the run.

ROUND 2
Seen in one second: the same headline; the matrix reads as a table with footnotes.
Scores: design 7.2 · usability 8 · creativity 6.5 · content 8.5 → overall 7.5
Caps applied: none.
Top three problems: the sideways matrix on phones, the run of split sections, and the long source URLs that wrap mid-path at 390. None is fixable without changing the page's structure.
```

## Benchmarks

```
ROUND 1
Seen in one second: DURABLE ACKS, MEASURED.
Idea (one sentence): the numbers are the picture; every loss is stated.
Live thing: the disk switch on the chart.
Layout words by section: hero=sentence / method=split list / disks=chart and tables / where we lose=split list / artifacts=list
Scores: design 7 · usability 8 · creativity 6.5 · content 9 → overall 7.5
Caps applied: none.
Top three problems (most visible first), each with the screenshot it shows in and the fix:
1. The chart's screen-reader table kept its full 412px width, so phones under 412px zoomed the page out (found at 320). Fix: a zero-height clipping box around it.
2. The section entrances relied on React; on a cold load with a reset JS chunk nothing rose in. Fix: the inline reveal script.
3. The 1,431-word page is long; the method list is the densest stretch (desktop-page-01). Copy is frozen for this task.

ROUND 2
Seen in one second: the same headline and the chart.
Scores: design 7.2 · usability 8 · creativity 6.5 · content 9 → overall 7.6
Caps applied: none.
Top three problems: the long method list, sideways tables on phones, and no photograph. All are the page's nature as a report.
```

## 404

```
ROUND 1
Seen in one second: NOTHING IN THE ATTIC.
Idea (one sentence): the attic is empty of this page, so it shows what is in it.
Live thing: none beyond the way home.
Layout words by section: hero=centred statement / photos=row of 4
Scores: design 7 · usability 8.5 · creativity 7 · content 7 → overall 7.4
Caps applied: none.
Top three problems (most visible first), each with the screenshot it shows in and the fix:
1. The page had no pictures and nothing moved (FAIL few-pictures, FAIL still-page). Fix: a row of the 4 archival photographs with credits, the masked line rise and the slow scale.
2. The margins differed at 768 (31px and 51px). Fix: the photo row spans the container and closes both margins.
3. The button lifted on hover, the one place on the site that did. Fix: the site's hover dim and press scale.

ROUND 2
Seen in one second: the headline over a row of 4 photographs.
Scores: design 7.5 · usability 8.5 · creativity 7 · content 7 → overall 7.6
Caps applied: none.
Top three problems: 8 moving elements (defended), the captions repeat the footer credits, and the row is the same crop for 4 different objects.
```

## The site as a whole

```
ROUND 2
Seen in one second: bone display caps on oxide, one signal-green action per screen.
Idea (one sentence): the sentence is the layout, and the proof sits inside it.
Live thing: the ack chip and Pull the plug on home; the search on home; the chart's disk switch on benchmarks.
Scores: design 7.6 · usability 8.2 · creativity 7.2 · content 8.3 → overall 7.7
Caps applied: none.
Top three problems:
1. The inner pages open on a type-only hero, so after home they read as one template; only the data below tells them apart.
2. The footer wordmark is set at 34.5vw (496px at 1440), the one size off the 5 steps and the display step; it is the footer's poster and stays.
3. No footage anywhere; the drift on the archival photographs is the only moving picture.
```
