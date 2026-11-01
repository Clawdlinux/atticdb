// Lets plain Node import the site's TypeScript modules: maps "@/x" to src/x and adds ".ts".
// Node 22 strips the types itself. No dependencies.
import { existsSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";

const SRC = new URL("../src/", import.meta.url);

export async function resolve(specifier, context, next) {
  let spec = specifier;
  if (spec.startsWith("@/")) spec = new URL(spec.slice(2), SRC).href;
  const isPath = spec.startsWith(".") || spec.startsWith("file:");
  if (isPath && !/\.[cm]?[jt]sx?$/.test(spec)) {
    const base = new URL(spec, context.parentURL);
    for (const ext of [".ts", ".tsx", "/index.ts"]) {
      const url = new URL(base.href + ext);
      if (existsSync(fileURLToPath(url))) return next(pathToFileURL(fileURLToPath(url)).href, context);
    }
  }
  return next(spec, context);
}
