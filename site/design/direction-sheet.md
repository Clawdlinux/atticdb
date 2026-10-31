# DIRECTION SHEET — The sentence is the layout
seed 328461324 · drawn 2026-10-31 · sector dev-tool · tone bold
Brief: AtticDB: an agent memory database in Rust. One local binary. A write is acked only after fdatasync. Open source today, hosted cloud next.

| | |
| --- | --- |
| Direction | The sentence is the layout |
| Hero | Controls on the sentence |
| Type | Bebas Neue / EB Garamond |
| Colour | Graphite and orange |
| Sections | intro (Pinned sentence, photographs pass) → showcase (Headline you can type) → how-it-works (Stages as one bar) → proof (Names as one sentence) → stats (Column aligned on the decimal) → pricing (One plan as a sentence) → faq (Large questions, answers beside) → cta (Question at both edges) → footer (Contact as the headline) |
| Motion | Masked headline |
| Photographs | the "Sky and space" set, as atmosphere |

This is the direction for this build. Adopt it. Do not blend it with the layout you would have made anyway,
and do not redraw because it is unfamiliar: unfamiliar is the point. Redraw a slot only when it conflicts
with a stated brand constraint or needs an asset that does not exist, and say why in your report.

The parts of this sheet were written separately and will disagree in small ways. Settle every disagreement in this order:
**house rules** (including type no smaller than 13px and body text of 16px or more) → **the direction** (the idea, the composition habits, where the
one accent goes) → **the type system and the palette** (which face does which job, the five size steps, which colour is the ground and where the
accent goes) → **the hero blueprint** → **the section blueprints**. Under each blueprint below, "Adjusted for this page" lists where it disagrees.
In practice: take geometry from blueprints (columns, proportions, what sits where) and take everything else from above them. Map every size a
blueprint names onto one of your five type steps instead of adding a sixth. Use one button shape, one corner radius and one pair of page margins on
the whole page, whatever individual blueprints say. If a blueprint names a kind of typeface (a light serif, a heavy grotesk), use the dealt face.

## 1. The idea
One short sentence, set at poster scale, is the composition. The trick that makes it an idea rather than a big headline: something from the subject sits inside the sentence where a word would be. A strip of photograph fills the gap between two words; a small live fragment of the product replaces the noun; the object the sentence names is pictured at the size of a letter. Write the sentence first, find the word that can be shown instead of said, and put the showing in its place. The reader completes the sentence with their eyes.

Before anything else, write one sentence in your notes: "On this page, <the subject's own material> becomes <the device>."
Fill the first blank from your inventory of what only this client has (its name and what it means, its street, its numbers, its
constraint), then put the nearest competitor in the sentence: if it still works, the idea is the category's, not the client's.
Then name the one live thing the visitor can do on the page (SKILL.md step 4).

## 2. Composition
The sentence is four to seven words and runs as one or two full-width lines at 9–13vw. Inline with the type, on the baseline, sits one media slot: a rounded-rectangle or square frame exactly one line-height tall and two to three ems wide, holding a photograph or a looping crop. Above and below the sentence the screen is empty except for a small navigation line and one supporting sentence set small, both on the same left margin. Later sections repeat the grammar at smaller scale: each is a short statement on the left six columns with its proof (image, figure, list) on the right, so the page reads as a series of sentences with evidence. Generous vertical space (160px or more) between sections.

## 3. Type — Bebas Neue / EB Garamond
Huge condensed capitals against a light old-style serif with italics: an industrial voice and a literary one, with a twenty-fold size gap between them. Suits crafted physical products, spirits, outdoor goods and hospitality.
```html
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Bebas+Neue:wght@400&family=EB+Garamond:ital,wght@0,400;0,500;1,400;1,500&display=swap" rel="stylesheet">
```
- Display: **Bebas Neue** — weight 400 (single cut, capitals only) · tracking -0.02em, tightening to -0.06em above 15vw · leading 0.85 · uppercase
- Text: **EB Garamond** — leading 1.45 · tracking 0
```css
:root {
  --step-small: clamp(0.8125rem, 0.79rem + 0.1vw, 0.875rem);
  --step-body: clamp(1.0625rem, 1rem + 0.25vw, 1.1875rem);
  --step-lead: clamp(1.25rem, 1.1rem + 0.7vw, 1.625rem);
  --step-head: clamp(2.5rem, 1.2rem + 5vw, 5.5rem);
}
```
Display weight with real presence (medium to bold), tracking -0.03 to -0.05em, leading 0.95–1.0 so two lines sit close. The inline frame aligns to cap height and baseline exactly; set it with em units so it scales with the type.
The direction decides sizes and how many of them; the type system decides how this face is set. Where the two give different tracking or leading, use the values of the type system: they are tuned to this face.
The display size itself comes from the hero blueprint below.

## 4. Colour — Graphite and orange
```css
:root {
  --bg: #18191b;
  --surface: #222427;
  --ink: #eff0f1;
  --muted: #9c9ea3;
  --accent: #ff4a1f;
  --accent-ink: #18191b;
  --field-1: #eff0f1;  --field-1-ink: #18191b;
}
```
Graphite and cool light-grey sections alternate, graphite taking about two thirds, and photographs are desaturated so nothing competes. Safety orange marks the buttons, one thick solid bar across the top of the page and the highlighted item in a list or chart.
A calm ground with ink at full contrast. The only colour comes from the photograph inside the sentence; if the palette has an accent, use it once, on the action.

## 5. Imagery
**This subject is software and there are no screenshots, so its pictures are its own interface.** Wherever this sheet says photograph, image or object, build a believable fragment of the product in HTML at real size (real labels, numbers that add up, one item hovered or selected) by the app-screen rules (references/app-screens.md: deal its style once with `node <skill>/scripts/draw.mjs --app --kind <kind> --keep palette,type --out .taste-skill/app`, and build it with assets/app.css and assets/charts.js), put the attribute `data-ui` on its root, and crop or scale it the way the blueprint treats a photograph: a full-bleed close crop, a tile, the fixed half of a split. Where a fragment makes no sense, use a flat colour field with type on it. Never a fake dashboard of grey bars, never a device frame. If the product works on something physical (ships, plants, buildings, a city), one real picture of that world can carry a section: find it with `node /Users/sunny/.agents/skills/design-taste-frontend/scripts/assets.mjs "<that world>"`, download it with `--get`, credit it on the page, and never let it pass for the product or its customers.

One strong, simple photograph that reads at small size (a face, an object, a texture, a horizon), cropped to a wide strip inside the sentence. The same image may open to full width in the next section, so the small slot feels like a window onto something larger.

**Floor (the inspector checks it):** at least four real pictures on the page, usually six to twelve; SVG drawings do not count. One moving picture where an honest clip exists: `node <skill>/scripts/assets.mjs --video "<query>"`, used as a muted loop with a poster frame. Whatever this direction says about pictures, it is carried out with real ones.

Optional atmosphere, two at most, from the "Sky and space" set (skies, clouds, stars and night skies). The page does not need them.
Order of preference for imagery: (1) the project's own photographs and product shots; (2) images you generate, if an image tool is
available; (3) real pictures of the subject's world found in the open collections (assets.mjs, downloaded and credited on the page);
(4) drawings of the subject in code; (5) these. They are real photographs (Unsplash licence, served by picsum.photos) of places, light and
texture. They are atmosphere, never a stand-in for a specific product, dish, person or interface. List the ones you use in your report so
they can be replaced.
- `https://picsum.photos/id/607/1600/1000` — Soft streaked clouds in a blue sky above a calm sea · mid tone · sky blue, white, teal · calm area: all · native portrait · photo: Grégoire Hervé-Bazin
- `https://picsum.photos/id/114/1600/1000` — Dark storm clouds hanging over a faint mountain ridge · dark tone · dark teal, muted beige · calm area: all · native landscape · photo: Brian Gonzalez
- `https://picsum.photos/id/38/1600/1000` — White cumulus cloud rising against a dark teal sky · mid tone · dark teal, white · calm area: top · native landscape · photo: Allyson Souza
- `https://picsum.photos/id/505/1600/1000` — Tiny silhouetted figure on a dark ridge under a blue and orange twilight sky · dark tone · deep blue, orange, black · calm area: top · native landscape · photo: lee Scott
The last two path numbers are width and height: request the exact crop you render (`/1200/1500` for a 4:5 portrait, `/2400/1350` for a hero) and
add `?grayscale` to desaturate. Use at most two. Always set `width`, `height`, `alt` and `loading="lazy"` (not on the hero image).
Other sets that fit this brief: fog-and-quiet, architecture, water — `node draw.mjs --photos <set>` prints one in full; `--photos list` shows all sets. Taking photographs from another set is your call and is not a redraw; keep them to one light and one treatment.

## 6. Hero — Controls on the sentence
A dark flat ground with no logo bar: a 14px wordmark top-left and the single action as an underlined text link top-right, both 28px in. The headline is a two-line sentence, centred, at clamp(96px, 9vw, 150px), line-height 1.0, occupying the middle 64% of the width and sitting slightly above the midline. It is set in a variable font and it is the demonstration. Three small control chips from the product sit on top of the sentence, each about 300 by 80px with the product's true radius, a text label and a numeric readout: one above the first line near its left end, one overlapping the start of the second line, and a two-state switch at the lower right. They are tilted 2 to 4 degrees and filled solid, one step lighter than the ground. Each one works: dragging the first changes the headline's weight through `font-variation-settings`, the second changes its letter-spacing, the third toggles uppercase. Mark each chip's root with data-ui. One supporting sentence at 17px, max 40ch, is centred 72px under the headline. The bottom third of the screen stays empty.

On a phone: The sentence stays on two lines at 13.5vw, centred at 38% of the height. The three chips shrink to 200 by 60px and move off the letters: one above the sentence and two side by side beneath it, untilted and still live. The supporting sentence follows at 16px. The wordmark stays top-left and the action link top-right.
Headline: A two-line sentence of three to five words, up to about 11 characters per line at 9vw, that still reads when its weight and spacing change.

Adjusted for this page:
- It names a kind of typeface: use the dealt faces (display Bebas Neue, text EB Garamond) and their settings; capitals only where the type system sets them.
- It sets a sentence large over several lines: jurors read that as a long headline. Either cut it to a two-line headline at display size, or keep it a statement at 36px or less on three lines (24px, four lines on a phone).
- Its action is a text link: the primary action on the first screen is a real button (filled or solid-bordered, 44px or taller), placed where the blueprint puts the link. A second action may stay a link.
- Its vw sizes assume a face of normal width: with this condensed display face, size the words to fill the measure the blueprint gives them instead of copying the vw value.

## 7. Sections, in order
Each is a composition to rebuild with this project's content. Keep the geometry; change the subject. Re-order or drop a
section when the argument of the page needs it (see references/copy.md), never to fall back to a card grid.

### 1. intro — Pinned sentence, photographs pass (pinned)
A wrapper 300svh tall. Inside it a `position: sticky; top: 0` layer of 100svh holds one sentence of 8–14 words, centred, three lines at clamp(36px, 4.6vw, 68px), leading 1.1, in a block 62% of the viewport wide. The sentence is a paragraph, not a heading; it is white with `mix-blend-mode: difference` (or a solid ink that reads over every photograph) and has a higher `z-index` than the images. Behind it, three photographs sit in normal flow at different sizes and sides and scroll past the fixed sentence: a 4:5 portrait 24vw wide with its left edge at 6vw; 70svh lower a 16:10 landscape 38vw wide touching the right margin; 70svh lower a 1:1 frame 18vw wide starting at 30vw. Each crosses the sentence once. When the wrapper ends, the sentence releases and scrolls away with the last photograph. No scroll hijacking.
On a phone: Wrapper 260svh. The sentence pins at 40% of the screen height at 8.4vw, four or five lines, with 20px margins. Photographs alternate: 64% wide on the left margin, 78% wide on the right margin, then 52% on the left, with 50svh between them.

Adjusted for this page:
- It names colours (white): map them onto this palette's roles (ground `--bg`, panels `--surface` and the fields, text `--ink`/`--muted`, the accent `--accent` on the primary action only).
- It sets a sentence large over several lines: jurors read that as a long headline. Either cut it to a two-line headline at display size, or keep it a statement at 36px or less on three lines (24px, four lines on a phone).
- It pins something for three screens or more: hold a pin for two or three screens at most, give every screen under it its own content, and let nothing pass over the pinned words (the inspector checks all three).
- It blends text with `difference`: over anything but black, white or grey that gives random hues. Use a solid ink chosen for each ground instead.
- Its vw sizes assume a face of normal width: with this condensed display face, size the words to fill the measure the blueprint gives them instead of copying the vw value.

### 2. showcase — Headline you can type (statement)
The section's headline is an input. One line at clamp(48px, 8vw, 132px), tight tracking, flush left across columns 1–12, is a real `<input>` (root marked data-ui) with no border and no fill, prefilled with a believable two-to-five-word example of what a user gives the product: a search, a question, a date in words, a colour name. A solid caret block 0.08em wide blinks after the last character. 48px beneath, on the same left edge, the product's answer appears at true interface size, at most 560px wide: three result rows, one parsed value, or a swatch, updating as the visitor types. Bottom-left, three more examples at 17px act as plain text buttons that refill the line. The rest of the 88svh field is empty. If the text outgrows the line it scrolls inside the input; it never wraps.
On a phone: The input is set at 11vw on one line and scrolls sideways inside itself when the text runs long. The answer follows 28px below at full width and true size. The three examples become a wrapping row of filled blocks 44px tall beneath the answer; tapping one refills the line.

Adjusted for this page:
- Its vw sizes assume a face of normal width: with this condensed display face, size the words to fill the measure the blueprint gives them instead of copying the vw value.

### 3. how-it-works — Stages as one bar (band)
The product's progress bar, enlarged until it spans the page, is the diagram. One bar runs across columns 1–12, 22vh tall, built as a flex row of three or four solid segments with 6px gaps, root marked data-ui. Each segment's width follows how long that stage really takes (flex-grow from the true durations, 12% minimum), and each is a different flat palette colour, so the reader sees at once that signing up is a sliver and the rest is the product working. Inside every segment, top-left with 20px padding, its duration in words at clamp(20px, 2vw, 30px): 'Two minutes', 'About a day'. Under each segment, on its left edge, one clause at 17px, 22ch wide, beginning with its order word. Above the bar, 64px clear of it, the heading: two to five words at clamp(48px, 6.4vw, 104px), flush left. Nothing else.
On a phone: The bar turns vertical: segments stack as full-width blocks whose heights keep the proportions, 72px minimum, 6px gaps, about 70svh in total. Each holds its duration at 22px top-left and its clause at 16px beneath. The heading sits above at 11vw on two lines.

Adjusted for this page:
- Its vw sizes assume a face of normal width: with this condensed display face, size the words to fill the measure the blueprint gives them instead of copying the vw value.

### 4. proof — Names as one sentence (statement)
Client, press or partner names are written out as one run-on sentence and set large: clamp(40px, 5vw, 76px), leading 1.0, flush left across all twelve columns, wrapping naturally over five to seven lines. Names are separated by commas and the list ends with a full stop. All names are set in a dimmed ink (still at least 3:1 against the ground), so the block reads as texture; one name at a time is lit in the full ink, and the lit name moves through the list every two seconds (a class toggled by a short script, with a 400ms colour transition). The sentence opens with two or three plain words in the full ink that act as the heading: the phrase that introduces the names. Nothing else is in the section: no logos, no columns, and 120px of space above and below.
On a phone: The same sentence at 9vw, wrapping over ten to fourteen lines; cut the list to the twelve strongest names so it stays within one and a half screens. The lit name still cycles. Margins are 20px and there are no other elements.

Adjusted for this page:
- It names a kind of typeface: use the dealt faces (display Bebas Neue, text EB Garamond) and their settings; capitals only where the type system sets them.
- It sets a sentence large over several lines: jurors read that as a long headline. Either cut it to a two-line headline at display size, or keep it a statement at 36px or less on three lines (24px, four lines on a phone).
- Its vw sizes assume a face of normal width: with this condensed display face, size the words to fill the measure the blueprint gives them instead of copying the vw value.

### 5. stats — Column aligned on the decimal (table)
A tall column of figures set like a ledger. Left, columns 1–5: the heading as a statement of up to five words on two lines at clamp(32px, 3.2vw, 52px), its first line in the soft ink and its second in full ink, placed at mid-height. Right, columns 7–12: four to six figures stacked, each at clamp(64px, 7vw, 112px), leading 1.0, in tabular lining numerals. They align on the decimal point: each row is a three-part grid in which the whole-number part is right-aligned in the first track, the point and decimals are left-aligned in the second, and a caption of one or two lines at 16px sits in the third, on the figure's baseline. Whole numbers simply leave the second track empty. Rows are separated by 20px of space only. Units are written in the captions, not attached to the figures, so the numerals form one clean vertical shape.
On a phone: The heading comes first at 28px. Figures drop to 15vw and keep the decimal alignment, with the split at 52% of the width. Each caption moves under its figure at 15px, starting from the left margin. 28px between rows.

Adjusted for this page:
- It sets a sentence large over several lines: jurors read that as a long headline. Either cut it to a two-line headline at display size, or keep it a statement at 36px or less on three lines (24px, four lines on a phone).
- Its vw sizes assume a face of normal width: with this condensed display face, size the words to fill the measure the blueprint gives them instead of copying the vw value.

### 6. pricing — One plan as a sentence (statement)
The whole offer is one sentence of five to seven short words that contains the price, set at 8–9vw across the full width on two lines, leading 0.95, flush left; it replaces any heading. The price is not styled as a different word. Instead it sits on a filled block of the accent colour exactly one line-height tall with 0.15em of padding at each side, so the figure reads as an object in the line. Below, on one shared baseline 64px under the sentence: a supporting sentence at 20px in columns 1–6 saying what the price includes, and one rectangular button right-aligned in columns 10–12. If a second option exists (annual, team), it is a plain text link under the button, not a second panel. The section is one flat colour field at least 80svh tall.
On a phone: The sentence is set at 11vw on two lines, with the price block on a line of its own between them, as wide as its figure. The supporting sentence follows at 17px, then a full-width button, then the text link for the second option.

Adjusted for this page:
- It sets a sentence large over several lines: jurors read that as a long headline. Either cut it to a two-line headline at display size, or keep it a statement at 36px or less on three lines (24px, four lines on a phone).
- Its vw sizes assume a face of normal width: with this condensed display face, size the words to fill the measure the blueprint gives them instead of copying the vw value.

### 7. faq — Large questions, answers beside (table)
Every answer is open; nothing collapses. Each question-and-answer pair is a row on the 12-column grid: the question in columns 1–6 at clamp(26px, 2.6vw, 40px), medium weight, leading 1.1, no more than two lines; its answer in columns 8–12 at 17–18px with leading 1.55 and a measure near 40ch, its first baseline level with the question's first baseline. Rows are separated by 72–88px of empty space and nothing else. Five to seven rows. The section heading, two or three words, is set at the same size as the questions but in the softer ink and sits as the first row's left cell with an empty right cell, so it reads as the top of the same table rather than a title above it. Because questions are large and answers small, the left column works as a scannable index.
On a phone: Each pair stacks: question at 24px, answer 12px beneath at 17px, then 56px of space before the next. The heading leads at the same 24px in the softer ink. Answers stay open; there is no accordion on the phone either.

Adjusted for this page:
- Its vw sizes assume a face of normal width: with this condensed display face, size the words to fill the measure the blueprint gives them instead of copying the vw value.

### 8. cta — Question at both edges (statement)
For a closing choice between two answers: book or ask, this date or that, the tasting or the tour. A flat field exactly 100svh tall in the darkest tone. The question is a two-line heading at clamp(110px, 11vw, 168px), leading 1.0, centred, about 54% of the width: line one with its cap line 32px under the field's top edge, line two with its baseline 24px above the bottom edge. The band of about 560px between them is empty except for one row at its optical centre: two solid rectangular buttons, each about 200×56px with its label at 18px in medium weight, their inner edges 120px either side of the centre axis, and between them, exactly on the axis, the word 'or' at 18px in the soft ink. 16px under the row, centred, one line at 16px in the soft ink says what happens after either choice. The type above and below closes the empty band, so it reads as held breath rather than a gap.
On a phone: The question drops to 18vw, still two centred lines, line one 32px from the top and line two 24px above the bottom. The buttons stack at the centre at 72% of the width, 12px apart, with 'or' between them and the line of explanation under the second.

Adjusted for this page:
- Its vw sizes assume a face of normal width: with this condensed display face, size the words to fill the measure the blueprint gives them instead of copying the vw value.

### 9. footer — Contact as the headline (statement)
A full-bleed field in one saturated palette colour, at least 90svh tall. Top row, 40px from the edge: two or three short notes at 15px in columns 1–7 (address, hours, a one-line description) and a back-to-top text link at the right margin. Then a tall empty gap. In the lower half, the way to get in touch is the display line: the telephone number or the email address itself, set at 8–9vw, flush left, hard against the margin, as one link. An email address may break once, before the @, giving two lines. Size it with `clamp()` so the longest line fills at least 80% of the width. Under it, one row of small text at 14px spread across three positions: links at the left, credits in the centre, city and year at the right. The wordmark appears only small, in the top row.
On a phone: Notes stack at the top at 15px. The contact line sits in the lower half at 12vw; a telephone number stays on one line, an email breaks before the @ and is sized so each half fits the width. The meta row becomes three stacked lines at 13px.

Adjusted for this page:
- Its vw sizes assume a face of normal width: with this condensed display face, size the words to fill the measure the blueprint gives them instead of copying the vw value.

## 8. Motion — Masked headline
On load, each line of the headline rises from behind its own baseline, about 900ms with a strong ease-out, the second line 110ms after the first; the supporting sentence and the action then fade up together. Nothing else on the page performs. Because headlines are two lines at most, you author the line breaks and wrap each line yourself. Recipe: 'Masked line reveal' in references/motion.md.
This is the signature move, the one loud moment. Around it the page still needs the rest of the motion layer (SKILL.md step 9): an entrance for the parts of every section, feedback on every control, and one moving picture. The inspector fails a page that barely moves. Recipes: references/motion.md.

## 10. Where the richness comes from
The collision of big type and one live image inside it. Because there is only one image on the first screen, choose it with care and give it perfect alignment to the letters.

## 11. How this direction goes wrong
The sentence must be short enough for two lines on a phone, where the inline frame drops to its own line between the two halves. Do not style one word differently to fake the device. Do not use more than one slot per sentence.

## 12. Real references
Studied sites that work this way. Take the principle, never the specifics: no names, copy, images or signature assets of theirs.
- **Spatial** (One Page Love, studied score 7.7) `opl-spatial`
  Idea: Typography behaves like sound in a room: plain black statements swell, stretch and bend through an empty white viewport as you scroll.
  Hero: A white viewport holding only the festival dates: two centred lines in Neue Haas Grotesk Roman at roughly 6.7vw, black, tight leading, sitting at the exact vertical middle and spanning about half the width. Large empty fields above and below; no image, no button, no supporting copy. The header is the only other element: a light uppercase wordmark with tight tracking flush to the top-left corner at a 20px inset, and nine small bold uppercase links in a row across the top right. Mobile is the same poster at 40px with a two-line hamburger.
  Worth taking: Open with a single factual line (the dates) centred on an otherwise empty white screen instead of a branded hero.
- **bleibtgleich’26** (Awwwards Site of the Day, studied score 7.5) `aww-bleibtgleich26`
  Idea: One thin vertical rule runs down the page and a tight bold grotesk sentence wraps around it, words stepping from one side of the line to the other.
  Hero: White page, black type. Content sits in a narrow band between 26% and 68% of the width. A 1px vertical rule drops from the top edge at 37% for two-thirds of the height. Left of it: a black blob avatar at the top, a tiny role caption at its foot. Right of it: a tiny location note at the top, the name in two lines of 90px medium grotesk at the bottom. The tagline continues below at the same size, flush to the band's left edge, running past the fold. The upper half is otherwise blank. The handle is split into two pale-grey words fixed in the bottom corners; a black menu pill sits top-right.
  Worth taking: Draw one 1px vertical rule and typeset a large sentence around it, alternating words to either side so the line becomes the text's spine.
- **Textmode Overlay** (One Page Love, studied score 7.7) `opl-textmode-overlay`
  Idea: The whole site is rendered in the product's medium: one bitmap face and live character-art fields, laid out like a terminal instrument panel with a numbered rail.
  Hero: Black viewport framed like an instrument panel. A 60px top bar with a hairline beneath: wordmark and version left, four small caps links right, a cream block button filling the bar's right end. A narrow right-edge rail lists 01 to 10 vertically; the active number extends leftward as an outlined label tab. The main area is a canvas of dim grey character marks with a few red ones, small caps captions in its top corners. The headline, two lines of cream pixel caps at 11vw, is left-aligned at mid-height, its last word in a slanted variant. Below a hairline, a bottom strip holds a short description left and two wide rectangular buttons.
  Worth taking: Add a fixed right-edge rail of section numbers where the active number sprouts a labelled tab into the content area.
Full notes on any of them: `node draw.mjs --site <id>`.

## Always
Headlines two lines or fewer at every width. No eyebrow labels, no monospace, no 01/02/03, no thin rules, no gradients, no shadows at rest,
no cream-and-terracotta, no violet. The frame: one h1 on the first screen that says what this is, equal outer margins, no
section huddled on one side with the other half empty, no blank stretch over half a screen, a heading on every section.
`inspect.mjs` measures all of it.

Saved to /Users/sunny/clawdlinux/atticdb/.taste-skill/direction.json · reprint with `node draw.mjs --show`
