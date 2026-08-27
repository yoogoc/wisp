// Imports @withfig/autocomplete into Wisp's RON completion specs.
//
// Every spec module is loaded with Node's own TypeScript support, so imports,
// spreads, and shared constants resolve exactly as Fig intends. Whatever is a
// JavaScript function -- `postProcess`, `custom`, dynamic `script`, `loadSpec`,
// `generateSpec`, `trigger` -- is recorded as a flag rather than dropped.

import { mkdirSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { emitSpec, emitPlaceholder, stats } from "./emit.mjs";

const source = new URL("./src/", import.meta.url).pathname;
const out = new URL("./out/", import.meta.url).pathname;

function walk(directory, files = []) {
  for (const entry of readdirSync(directory)) {
    const path = join(directory, entry);
    if (statSync(path).isDirectory()) walk(path, files);
    else if (path.endsWith(".ts")) files.push(path);
  }
  return files;
}

/// A versioned root exports a function that names the spec file to use; ask it
/// for an impossibly high version so it answers with the newest it ships.
async function resolveSpec(spec) {
  if (typeof spec !== "function") return spec;
  const location = await spec("999.999.999");
  if (!location?.versionedSpecPath) return undefined;
  return (await import(join(source, `${location.versionedSpecPath}.ts`))).default;
}

const failures = [];
let placeholders = 0;
const files = walk(source).sort();

for (const file of files) {
  const id = relative(source, file).replace(/\.ts$/, "").replace(/\/index$/, "");
  const target = join(out, `${id}.ron`);
  mkdirSync(dirname(target), { recursive: true });
  let document;
  try {
    const spec = await resolveSpec((await import(file)).default);
    const name = Array.isArray(spec?.name) ? spec.name[0] : spec?.name;
    if (typeof name === "string" && name) {
      document = emitSpec(spec, name);
    } else {
      placeholders += 1;
      document = emitPlaceholder(id.split("/").pop());
    }
  } catch (error) {
    failures.push([id, String(error).split("\n")[0].slice(0, 160)]);
    placeholders += 1;
    document = emitPlaceholder(id.split("/").pop());
  }
  writeFileSync(target, document);
}

const report = {
  source: "withfig/autocomplete 2.692.3",
  source_commit: "aef52acff84c45edde61ae610cc2c964802b9a38",
  indexed: files.length,
  imported: files.length - placeholders,
  placeholders,
  failed: failures.length,
  ...stats,
};
writeFileSync(join(out, "..", "import-report.json"), JSON.stringify(report, null, 2));
console.log(JSON.stringify(report, null, 2));
for (const [id, message] of failures.slice(0, 20)) console.log("FAIL", id, message);
