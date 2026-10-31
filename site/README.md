# AtticDB site

Static marketing site for [atticdb.com](https://atticdb.com).
Built with Next.js 16, React, CSS modules, and Motion.

## Development

Use Node.js 22. Run commands from `site/`:

```sh
npm ci
npm run dev
```

Open <http://localhost:3000>.

## Validation

```sh
npm test
npm run lint
npm run build
```

The build exports static files to `out/`. Serve that directory with a static web server.
The build downloads Google Fonts. The exported site serves font files locally.

## Structure

- `src/app/`: root layout, global tokens, and pages.
- `src/components/`: shared navigation and motion provider.
- `src/lib/site.ts`: public URLs and the install command.
- `design/`: design references.

The homepage is a placeholder. Navigation destinations will land in later tasks.
