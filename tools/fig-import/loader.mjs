import { existsSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";

const CANDIDATES = ["", ".ts", ".js", ".mjs", "/index.ts", "/index.js", "/index.mjs"];
const requireFromImporter = createRequire(import.meta.url);
const IMPORTER_DEPENDENCIES = new Set([
  "@fig/autocomplete-generators",
  "@fig/autocomplete-helpers",
  "semver",
  "strip-json-comments",
  "typescript",
  "yaml",
]);

/// TypeScript specs import each other without a file extension, and import a
/// directory to mean its index.ts; Node ESM resolves neither.
export async function resolve(specifier, context, next) {
  if (specifier === "@fig/autocomplete-generators") {
    return next(new URL("./shim-generators.mjs", import.meta.url).href, context);
  }
  if (IMPORTER_DEPENDENCIES.has(specifier)) {
    return next(pathToFileURL(requireFromImporter.resolve(specifier)).href, context);
  }
  if (specifier.startsWith(".") || specifier.startsWith("/")) {
    try {
      const path = fileURLToPath(new URL(specifier, context.parentURL));
      for (const candidate of CANDIDATES.map((suffix) => path + suffix)) {
        if (existsSync(candidate) && statSync(candidate).isFile()) {
          return next(pathToFileURL(candidate).href, context);
        }
      }
    } catch {
      // fall through to the default resolver
    }
  }
  return next(specifier, context);
}
