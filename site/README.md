# AtticDB site

Static site for [atticdb.com](https://atticdb.com).
Built with Next.js 16, React 19, CSS modules and Motion.

## Development

Use Node.js 22. Run every command below from `site/`:

```sh
npm ci
npm run dev
```

Open <http://localhost:3000>.

## Test and build

```sh
npm test
npm run lint
npm run build
```

The build exports static files to `out/`. No Node.js server runs in production.
The build downloads Google Fonts. The exported site serves those fonts locally.
Before `dev` and `build`, `npm run poster` writes `public/tape-poster.svg`, the hero tape's still frame.
It runs the tape sim with plain Node through `scripts/ts-hooks.mjs`. The file is git-ignored.

## Preview

```sh
npx serve out
```

Open the address printed by `serve`. Its clean URLs map `/pricing` to `pricing.html`.
Stop the server with Ctrl+C.
The preview does not apply Vercel's host rules or security headers.
Docs are available at `/docs` locally.

## Deploy

Use 2 Vercel projects. Both use `site/` as their root.
Build and test first. Deploy the main project from the repository root:

```sh
cd site && vercel link --project atticdb-site && vercel deploy --prod
```

`atticdb-site` uses `vercel.json`, builds with `npm run build`, and serves `out/`.
`www.atticdb.com` redirects to `atticdb.com`.
Both main domains redirect `/docs` and `/docs/*` to `docs.atticdb.com`.

### Docs project

Never connect `atticdb-docs` to Git. A Git build reads `vercel.json` and would build the main site.

`atticdb-docs` uses `vercel.docs.json`, builds with `npm run build:docs`, and serves `out-docs/`.
The build copies docs pages, their sitemap and robots file, and shared assets into that separate output.
Main-only pages are excluded. Vercel checks static files before rewrites, so host rewrites cannot replace the main homepage.

Vercel's remote build always reads `vercel.json`, so `--local-config` doesn't switch the build.
`scripts/deploy-docs.sh` stages a copy of `site/` with `vercel.docs.json` as its `vercel.json`, then deploys it.
It finds the docs project id with `vercel project inspect atticdb-docs`. Deploy from `site/`:

```sh
scripts/deploy-docs.sh --prod
```

Set the domains on their respective projects:

```sh
vercel domains add atticdb.com atticdb-site
vercel domains add www.atticdb.com atticdb-site
vercel domains add docs.atticdb.com atticdb-docs
```

Preview builds link shared navigation to production `https://atticdb.com`.
For a local docs-project preview, run `VERCEL=1 npm run build:docs`, then `npx serve out-docs`.
Normal local development keeps docs at `/docs`.
The docs host redirects `/docs` to `/` and strips `/docs/` from nested paths.

### Cloudflare DNS

Keep DNS on Cloudflare. Set all 3 records to **DNS only**, without the proxy.
Vercel issues the certificates.
These are the records `vercel domains inspect` asks for. They were added on 2026-11-01 and the site is live.
Vercel also lists newer optional values (`216.198.79.1` or a project CNAME). The A records above work, so there's no need to switch.
If HTTPS fails after DNS resolves, run `vercel certs issue atticdb.com www.atticdb.com docs.atticdb.com`.

| Type | Name | Content | Proxy |
| --- | --- | --- | --- |
| A | @ | 76.76.21.21 | DNS only |
| A | www | 76.76.21.21 | DNS only |
| A | docs | 76.76.21.21 | DNS only |

Wrangler can't write DNS because its OAuth scopes stop at `zone:read`.
Create a Cloudflare API token with Zone DNS Edit for `atticdb.com`.
Save the token outside the repository. Never commit it.
From `site/`, run `scripts/cf-dns.sh <token-file>` to create these records through the Cloudflare API.
`cf-dns.sh` skips records that already match and stops on proxied or conflicting ones.

### Headers

Both Vercel configs set identical CSP and security headers.
Hashed assets under `/_next/static/` receive immutable caching.
Inline scripts and styles are allowed because Next.js exports them.
Fonts and images are hosted locally.

## CI

`.github/workflows/site.yml` runs on pull requests to `main` and pushes to `main`.
It triggers when `site/` or the workflow changes.
It uses Node.js 22 and runs `npm ci`, `npm test`, `npm run lint` and `npm run build`.
CI validates the export. It does not deploy.

## Content and sources

Edit `site/design/competitors-*.md` and `site/src/content/compare.ts` together when refreshing competitor facts.
These paths start at the repository root. Record the retrieval date and the product's own source URL.
Keep unpublished claims marked `Not published`. Tests enforce source URLs for published competitor claims.
Run `npm test` after editing.

Image credits live in `src/content/credits.ts` and appear in the site footer.
Archival images live in `public/images/`.

### Archival images

The originals are `public/images/<name>.jpg`. They are the source of truth and the keys in `credits.ts`.
The site never serves them. It serves resized copies: `<name>-<w>.avif` and `<name>-<w>.jpg`.
Widths are 480, 800 and the original width, capped at 1200.
After adding or replacing an original, regenerate the copies on macOS:

```sh
sh scripts/make-images.sh
```

The script uses `sips` only. Commit the copies with the original.
Render a picture with `src/components/ArchivalPicture.tsx`. Give it a `sizes` that matches its rendered width.

## Social image

`public/og.png` is the shared 1200×630 image.
Refresh it after changing the hero:

```sh
./scripts/make-og.sh
```

The script needs Python 3 and Google Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`.
Set `CHROME_BIN` to use another Chrome path. The screenshot has a 60-second timeout.
`scripts/og.html` uses local fonts from `scripts/fonts/`. It does not load web assets during capture.
The fonts were copied from Next.js build output. Their OFL licenses sit beside them.
Inspect `public/og.png` after rendering. Rebuild the site to copy it into `out/`.

## Structure

- `src/app/` contains pages, the layout and static sitemap and robots routes.
- Metadata and structured data helpers live in `src/lib/seo.ts`.
- `src/components/` holds shared navigation, motion and page components.
- Public URLs and the install command live in `src/lib/site.ts`.
- `design/` keeps design references and competitor evidence.
