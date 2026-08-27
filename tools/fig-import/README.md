# Fig spec importer

Regenerates `specs/` from a checkout of
[`withfig/autocomplete`](https://github.com/withfig/autocomplete).

```sh
cd tools/fig-import
npm install
node --experimental-transform-types --max-old-space-size=8192 \
     --import ./register.mjs import.mjs /path/to/autocomplete
rsync -a --delete --exclude README.md --exclude LICENSE-FIG ./out/ ../../specs/
```

Each spec module is loaded with Node's own TypeScript support rather than being
parsed, so imports, spreads, and shared constants resolve exactly as Fig
intends. The checkout path and optional output directory are command-line
arguments; Fig's source is never copied into Wisp. `emit.mjs` then writes
Wisp's RON schema, omitting any field that sits at its Rust default.

Whatever Fig expressed as a JavaScript function -- `postProcess`, `custom`, a
dynamic `script`, `loadSpec`, `generateSpec`, `trigger`, `getQueryTerm` -- is
recorded as a `has_*` flag instead of being dropped, so the engine can tell
"nothing here" apart from "not expressible without a JavaScript runtime".

`loader.mjs` resolves the extensionless and directory imports the specs use, and
supplies the one helper the published `@fig/autocomplete-generators` is missing.
`import.mjs` prints a coverage report; paste it into `specs/README.md`.
