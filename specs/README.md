# Completion specs

Every file in this tree is one data-only RON command specification, and its path
below `specs/` is its spec id: `git.ron` holds the spec `git`, and
`az/2.53.0/network.ron` holds `az/2.53.0/network`. Files that are not `.ron` --
this README and `LICENSE-FIG` -- are ignored.

`crates/wisp-core/build.rs` walks the tree, compresses each document on its own,
and concatenates them into a container the binary embeds; the runtime inflates a
document the first time its command is completed, so the roughly 240 MB data set
is never deserialized at startup. To read the top-level `command` name cheaply
the build script scans only the head of each file, so that field must stay above
`subcommands` and `options`.

## Provenance

The tree is a snapshot derived from `@withfig/autocomplete` 2.692.3 at commit
`aef52acff84c45edde61ae610cc2c964802b9a38`, under the MIT notice in
`LICENSE-FIG`. Wisp does not ship or execute the original
TypeScript/JavaScript modules. Static command trees, options, arguments,
suggestions, path templates, aliases, and `loadSpec` references are retained.

Fig callbacks and shell generators are represented as unavailable generator
metadata unless Wisp has a separately reviewed Rust adapter. This keeps
completion specs data-only and prevents imported rules from executing arbitrary
commands.

Import coverage of that snapshot:

```ron
(
    source_commit: "aef52acff84c45edde61ae610cc2c964802b9a38",
    indexed: 1484,
    imported: 1481,
    failed: 0,
    placeholders: 3,
    commands: 1484,
    subcommands: 50784,
    options: 282837,
    arguments: 235174,
    static_suggestions: 123596,
    path_arguments: 4817,
    generators: 4948,
    dynamic_trees: 0,
    versioned_roots_resolved: 6,
)
```
