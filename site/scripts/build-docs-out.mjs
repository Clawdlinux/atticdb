import { cpSync, existsSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * @param {string} outDir
 * @param {string} docsOutDir
 */
export function buildDocsOut(outDir, docsOutDir) {
  rmSync(docsOutDir, { recursive: true, force: true });
  mkdirSync(docsOutDir, { recursive: true });
  cpSync(join(outDir, "docs.html"), join(docsOutDir, "index.html"));
  if (existsSync(join(outDir, "docs.txt"))) {
    cpSync(join(outDir, "docs.txt"), join(docsOutDir, "index.txt"));
  }

  const docsDir = join(outDir, "docs");
  for (const entry of readdirSync(docsDir, { withFileTypes: true })) {
    // .md: the Markdown copy of each page, for agents (scripts/build-agent-files.mjs).
    if (entry.isDirectory() || /\.(html|txt|md)$/.test(entry.name) || entry.name === "sitemap.xml") {
      cpSync(join(docsDir, entry.name), join(docsOutDir, entry.name), { recursive: true });
    }
  }

  // llms.txt is the same index on both hosts.
  for (const asset of ["_next", "images", "fonts", "icon.svg", "og.png", "404.html", "llms.txt"]) {
    const source = join(outDir, asset);
    if (existsSync(source)) cpSync(source, join(docsOutDir, asset), { recursive: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  buildDocsOut(
    fileURLToPath(new URL("../out", import.meta.url)),
    fileURLToPath(new URL("../out-docs", import.meta.url)),
  );
}
