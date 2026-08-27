import { existsSync, statSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";

const CANDIDATES = ["", ".ts", ".js", ".mjs", "/index.ts", "/index.js", "/index.mjs"];

/// TypeScript specs import each other without a file extension, and import a
/// directory to mean its index.ts; Node ESM resolves neither.
export async function resolve(specifier, context, next) {
  if (specifier === "@fig/autocomplete-generators" && context.parentURL?.endsWith("/scc.ts")) {
    return next(new URL("./shim-generators.mjs", import.meta.url).href, context);
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
