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

## Preview

```sh
npx serve out
```

Open the address printed by `serve`. Its clean URLs map `/pricing` to `pricing.html`.
Stop the server with Ctrl+C.
The preview does not apply Cloudflare's `_headers` rules.

## Deploy

Build and test first. Then run:

```sh
npx wrangler login
npx wrangler pages deploy out --project-name atticdb-site
```

Create the `atticdb-site` Pages project if Wrangler prompts you.
Add `atticdb.com` and `www.atticdb.com` as custom domains in that Pages project.
Set a Cloudflare redirect from `www.atticdb.com` to `atticdb.com`. Preserve the path and query string.
`wrangler.toml` points Pages at `out/`.

`public/_headers` sets the CSP and security headers. Hashed assets under `/_next/static/` receive immutable caching.
Inline scripts and styles are allowed because Next.js exports them. Fonts and images are hosted locally.

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
