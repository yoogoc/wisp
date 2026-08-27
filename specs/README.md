# Completion specs

Every file in this tree is one data-only RON command specification, and its path
below `specs/` is its spec id: `git.ron` holds the spec `git`, and
`az/2.53.0/network.ron` holds `az/2.53.0/network`. Files that are not `.ron` --
this README and `LICENSE-FIG` -- are ignored.

`crates/wisp-core/build.rs` walks the tree, compresses each document on its own,
and concatenates them into a container the binary embeds; the runtime inflates a
document the first time its command is completed, so the roughly 190 MB data set
is never deserialized at startup. To read the top-level `command` name cheaply
the build script scans only the head of each file, so that field must stay above
`subcommands` and `options`.

## Provenance

The tree is a snapshot derived from `@withfig/autocomplete` 2.692.3 at commit
`aef52acff84c45edde61ae610cc2c964802b9a38`, under the MIT notice in
`LICENSE-FIG`. Wisp does not ship or execute the original
TypeScript/JavaScript modules. Static command trees, options, arguments,
suggestions, path templates, aliases, and `loadSpec` references are retained.

Every field a Fig spec carries declaratively is kept. Whatever Fig expressed as
a JavaScript function -- `postProcess`, `custom`, a dynamic `script`,
`loadSpec`, `generateSpec`, `trigger`, `getQueryTerm` -- is recorded as a
`has_*` flag rather than dropped, so the engine can tell "nothing here" apart
from "not expressible without a JavaScript runtime".

A generator keeps the argv Fig would have run;
`crates/wisp-core/src/generators.ron` says how to read each script's output and
which programs may be spawned at all. Specs therefore stay data-only and cannot
make Wisp execute an arbitrary command.

Run `tools/fig-import` to regenerate this tree from a newer snapshot.

Import coverage of that snapshot, from `tools/fig-import`:

```ron
(
    source: "withfig/autocomplete 2.692.3",
    source_commit: "aef52acff84c45edde61ae610cc2c964802b9a38",
    indexed: 1484,
    imported: 1478,
    placeholders: 6,
    failed: 0,
    commands: 1484,
    subcommands: 51933,
    options: 284155,
    arguments: 236318,
    suggestions: 123654,
    generators: 5315,
    scripts: 3574,
    dynamic_scripts: 798,
    post_process: 4313,
    custom: 854,
    templates: 5299,
    load_specs: 984,
    dynamic_load_specs: 2,
    generate_specs: 37,
    triggers: 134,
    dynamic_triggers: 1004,
    query_terms: 142,
    dynamic_query_terms: 935,
    caches: 1366,
    parser_directives: 167,
    exclusive_on: 1892,
    depends_on: 335,
    persistent_options: 622,
    required_options: 17417,
    repeatable_options: 932,
    separators: 4541,
    hidden: 2036,
    priorities: 4516,
    icons: 22872,
    display_names: 1726,
    insert_values: 2693,
    dangerous: 356,
    string_scripts: 0,
    truncated: 0,
)
```
