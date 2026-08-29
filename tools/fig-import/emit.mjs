// Serialises a Fig completion spec object into Wisp's RON schema.
// Fields at their Rust default are omitted so the documents stay readable.

import { generatorMetadata, nativeForPath } from "./generator-metadata.mjs";

export const stats = {
  commands: 0, subcommands: 0, options: 0, arguments: 0, suggestions: 0,
  generators: 0, scripts: 0, dynamic_scripts: 0, post_process: 0, custom: 0,
  templates: 0, load_specs: 0, dynamic_load_specs: 0, generate_specs: 0,
  triggers: 0, dynamic_triggers: 0, query_terms: 0, dynamic_query_terms: 0,
  caches: 0, parser_directives: 0, exclusive_on: 0, depends_on: 0,
  alias_directives: 0, dynamic_aliases: 0,
  persistent_options: 0, required_options: 0, repeatable_options: 0,
  separators: 0, hidden: 0, priorities: 0, icons: 0, display_names: 0,
  insert_values: 0, dangerous: 0, string_scripts: 0, truncated: 0,
};

const MAX_DEPTH = 32;

function text(value) {
  let out = "";
  for (const character of String(value)) {
    const code = character.codePointAt(0);
    if (character === "\\") out += "\\\\";
    else if (character === '"') out += '\\"';
    else if (character === "\n") out += "\\n";
    else if (character === "\r") out += "\\r";
    else if (character === "\t") out += "\\t";
    else if (code < 0x20 || code === 0x7f) out += `\\u{${code.toString(16)}}`;
    else out += character;
  }
  return `"${out}"`;
}

function record(fields, indent) {
  const kept = fields.filter(([, value]) => value !== undefined);
  if (kept.length === 0) return "()";
  const pad = " ".repeat(indent + 4);
  return `(\n${kept.map(([key, value]) => `${pad}${key}: ${value},\n`).join("")}${" ".repeat(indent)})`;
}

function list(items, indent) {
  if (items.length === 0) return undefined;
  const pad = " ".repeat(indent + 4);
  return `[\n${items.map((item) => `${pad}${item},\n`).join("")}${" ".repeat(indent)}]`;
}

const flag = (value) => (value ? "true" : undefined);
const some = (value) => (value === undefined || value === null || value === "" ? undefined : `Some(${text(value)})`);
const number = (value) => (typeof value === "number" && Number.isFinite(value) ? `Some(${Number.isInteger(value) ? `${value}.0` : value})` : undefined);
const array = (value) => (value === undefined ? [] : Array.isArray(value) ? value : [value]);

const TEMPLATES = { filepaths: "FilePaths", folders: "Folders", history: "History", help: "Help" };
const FILTER = { fuzzy: "Fuzzy", prefix: "Prefix", default: undefined };
const TYPES = {
  folder: "Folder", file: "File", arg: "Arg", subcommand: "Subcommand",
  option: "Option", special: "Special", mixin: "Mixin", shortcut: "Shortcut",
};

function templates(value, indent) {
  const items = array(value).map((name) => TEMPLATES[name]).filter(Boolean);
  if (items.length) stats.templates += items.length;
  return list(items, indent);
}

function presentation(node, indent) {
  if (node.hidden) stats.hidden += 1;
  if (typeof node.priority === "number") stats.priorities += 1;
  if (node.icon) stats.icons += 1;
  if (node.displayName) stats.display_names += 1;
  if (node.insertValue) stats.insert_values += 1;
  if (node.isDangerous) stats.dangerous += 1;
  const fields = [
    ["priority", number(node.priority)],
    ["hidden", flag(node.hidden)],
    ["icon", some(node.icon)],
    ["display_name", some(node.displayName)],
    ["insert_value", some(node.insertValue ?? node.replaceValue)],
    ["dangerous", flag(node.isDangerous)],
  ].filter(([, value]) => value !== undefined);
  return fields.length ? record(fields, indent) : undefined;
}

function suggestion(value, indent) {
  stats.suggestions += 1;
  if (typeof value === "string") return record([["name", text(value)]], indent);
  const names = array(value.name);
  return record([
    ["name", text(names[0] ?? "")],
    ["aliases", list(names.slice(1).map(text), indent + 4)],
    ["description", some(value.description)],
    ["kind", TYPES[value.type] ? `Some(${TYPES[value.type]})` : undefined],
    ["presentation", presentation(value, indent + 4)],
  ], indent);
}

function trigger(value, indent) {
  if (typeof value === "function") { stats.dynamic_triggers += 1; return ["has_dynamic_trigger", "true"]; }
  stats.triggers += 1;
  if (typeof value === "string") return ["trigger", `Some(OnMatch(${list([text(value)], indent + 4) ?? "[]"}))`];
  if (value.on === "change") return ["trigger", "Some(OnChange)"];
  if (value.on === "threshold") return ["trigger", `Some(OnThreshold(${value.length ?? 0}))`];
  if (value.on === "match") {
    const strings = array(value.string).map(text);
    return ["trigger", `Some(OnMatch(${list(strings, indent + 4) ?? "[]"}))`];
  }
  return ["trigger", undefined];
}

function queryTerm(value) {
  if (typeof value === "function") { stats.dynamic_query_terms += 1; return ["has_dynamic_query_term", "true"]; }
  stats.query_terms += 1;
  return ["query_term", `Some(AfterLast(${text(value)}))`];
}

function cache(value, indent) {
  if (!value || typeof value !== "object") return undefined;
  stats.caches += 1;
  return record([
    ["ttl_ms", typeof value.ttl === "number" ? `Some(${Math.round(value.ttl)})` : undefined],
    ["by_directory", flag(value.cacheByDirectory)],
    ["stale_while_revalidate", value.strategy === "max-age" ? undefined : "true"],
  ], indent);
}

function aliasResolver(value, path, indent) {
  if (value === undefined) return undefined;
  stats.alias_directives += 1;
  const commandPath = path.join(" ");
  let fields;
  if (commandPath === "git") {
    fields = [
      ["script", list(["git", "--no-optional-locks", "config", "--get", "alias.{alias}"].map(text), indent + 4)],
      ["reject_prefixes", list([text("!")], indent + 4)],
    ];
  } else if (commandPath === "gh") {
    fields = [
      ["script", list(["gh", "alias", "list"].map(text), indent + 4)],
      ["pipeline", 'Lines((delimiter: Some(":"), name: 0, description: Some(From(1))))'],
      ["selection", "MatchingName"],
      ["value", "Description"],
      ["reject_prefixes", list([text("!")], indent + 4)],
    ];
  } else if (["yarn", "yarn run", "rushx"].includes(commandPath)) {
    fields = [
      ["native", "Some(PackageJsonScripts)"],
      ["selection", "MatchingName"],
      ["value", "Description"],
    ];
  } else {
    stats.dynamic_aliases += 1;
    return undefined;
  }
  return `Some(${record(fields, indent)})`;
}

function parserDirectives(value, indent, path) {
  if (!value || typeof value !== "object") return undefined;
  const separators = array(value.optionArgSeparators).filter((item) => typeof item === "string");
  const fields = [
    ["flags_are_posix_noncompliant", flag(value.flagsArePosixNoncompliant)],
    ["options_must_precede_arguments", flag(value.optionsMustPrecedeArguments)],
    ["option_arg_separators", list(separators.map(text), indent + 4)],
    ["alias", aliasResolver(value.alias, path, indent + 4)],
  ].filter(([, item]) => item !== undefined);
  if (!fields.length) return undefined;
  stats.parser_directives += 1;
  return `Some(${record(fields, indent)})`;
}

function generator(value, indent) {
  stats.generators += 1;
  const fields = [];
  const script = value.script;
  let argv;
  if (typeof script === "function") { stats.dynamic_scripts += 1; fields.push(["has_dynamic_script", "true"]); }
  else if (Array.isArray(script) && script.length) {
    argv = script;
  } else if (typeof script === "string" && script.trim()) {
    stats.string_scripts += 1;
    argv = script.trim().split(/\s+/);
  } else if (script && typeof script === "object") {
    argv = [script.command, ...array(script.args)].filter((word) => typeof word === "string");
    if (script.cwd) fields.push(["cwd", some(script.cwd)]);
    if (typeof script.timeout === "number") fields.push(["script_timeout_ms", `Some(${Math.round(script.timeout)})`]);
  }
  const metadata = argv?.length ? generatorMetadata(argv) : undefined;
  if (argv?.length) {
    stats.scripts += 1;
    fields.push(["script", list(argv.map(text), indent + 4)]);
  }
  if (metadata?.pipeline) fields.push(["pipeline", metadata.pipeline]);
  if (metadata?.kind) fields.push(["kind", metadata.kind]);
  if (metadata?.rejectPrefixes.length) {
    fields.push(["reject_prefixes", list(metadata.rejectPrefixes.map(text), indent + 4)]);
  }
  if (typeof value.scriptTimeout === "number") fields.push(["script_timeout_ms", `Some(${Math.round(value.scriptTimeout)})`]);
  if (value.splitOn) fields.push(["split_on", some(value.splitOn)]);
  const template = templates(value.template, indent + 4);
  if (template) fields.push(["template", template]);
  if (typeof value.filterTemplateSuggestions === "function") fields.push(["has_filter_template_suggestions", "true"]);
  if (typeof value.postProcess === "function") { stats.post_process += 1; fields.push(["has_post_process", "true"]); }
  if (typeof value.custom === "function") { stats.custom += 1; fields.push(["has_custom", "true"]); }
  if (value.trigger !== undefined) fields.push(trigger(value.trigger, indent + 4));
  if (value.getQueryTerm !== undefined) fields.push(queryTerm(value.getQueryTerm));
  const cached = cache(value.cache, indent + 4);
  if (cached) fields.push(["cache", `Some(${cached})`]);
  return record(fields.filter(([, item]) => item !== undefined), indent);
}

function argument(value, indent, depth, path) {
  stats.arguments += 1;
  const suggestions = array(value.suggestions).map((item) => suggestion(item, indent + 8));
  const generatorValues = array(value.generators).filter(Boolean);
  const liftedTemplates = generatorValues
    .map((item) => item.__wispTemplate)
    .filter(Boolean);
  const generators = generatorValues
    .filter((item) => !item.__wispTemplate)
    .map((item) => generator(item, indent + 8));
  const load = loadSpec(value.loadSpec, indent + 4, depth, path);
  const native = generatorValues.some((item) => !item.script)
    ? nativeForPath(path)
    : undefined;
  return record([
    ["name", text(value.name ?? "")],
    ["description", some(value.description)],
    ["suggestions", list(suggestions, indent + 4)],
    ["template", templates([...array(value.template), ...liftedTemplates], indent + 4)],
    ["generators", list(generators, indent + 4)],
    ["optional", flag(value.isOptional)],
    ["variadic", flag(value.isVariadic)],
    ["options_can_break_variadic", flag(value.optionsCanBreakVariadicArg)],
    ["filter_strategy", FILTER[value.filterStrategy]],
    ["suggest_current_token", flag(value.suggestCurrentToken)],
    ["is_command", flag(value.isCommand)],
    ["is_script", flag(value.isScript)],
    ["is_module", some(value.isModule)],
    ["debounce", flag(value.debounce)],
    ["default", some(value.default)],
    ...load,
    ["dangerous", flag(value.isDangerous)],
    ["parser_directives", parserDirectives(value.parserDirectives, indent + 4, path)],
    ["native", native ? `Some(${native})` : undefined],
  ], indent);
}

function option(value, indent, depth, path) {
  stats.options += 1;
  const names = array(value.name);
  const args = array(value.args).filter(Boolean).map((item) => argument(item, indent + 8, depth, path));
  if (value.isPersistent) stats.persistent_options += 1;
  if (value.isRequired) stats.required_options += 1;
  if (value.isRepeatable) stats.repeatable_options += 1;
  if (value.exclusiveOn?.length) stats.exclusive_on += 1;
  if (value.dependsOn?.length) stats.depends_on += 1;
  let separator;
  if (typeof value.requiresSeparator === "string") separator = some(value.requiresSeparator);
  else if (value.requiresSeparator === true || value.requiresEquals === true) separator = `Some("=")`;
  if (separator) stats.separators += 1;
  let repeat;
  if (typeof value.isRepeatable === "number") repeat = `Times(${value.isRepeatable})`;
  else if (value.isRepeatable === true) repeat = "Many";
  return record([
    ["names", list(names.map(text), indent + 4) ?? "[]"],
    ["description", some(value.description)],
    ["arguments", list(args, indent + 4)],
    ["persistent", flag(value.isPersistent)],
    ["required", flag(value.isRequired)],
    ["repeat", repeat],
    ["requires_separator", separator],
    ["exclusive_on", list(array(value.exclusiveOn).map(text), indent + 4)],
    ["depends_on", list(array(value.dependsOn).map(text), indent + 4)],
    ["presentation", presentation(value, indent + 4)],
  ], indent);
}

function loadSpec(value, indent, depth, path) {
  if (value === undefined || value === null) return [];
  if (typeof value === "string") { stats.load_specs += 1; return [["load_spec", some(value)]]; }
  if (typeof value === "function") { stats.dynamic_load_specs += 1; return [["has_dynamic_load_spec", "true"]]; }
  if (typeof value === "object") {
    stats.load_specs += 1;
    return [["load_spec_inline", `Some(${subcommand(value, indent, depth + 1, path)})`]];
  }
  return [];
}

function body(node, indent, depth, path) {
  const subcommands = array(node.subcommands).filter(Boolean).map((item) => {
    const name = array(item.name)[0] ?? "";
    return subcommand(item, indent + 8, depth + 1, [...path, name]);
  });
  const options = array(node.options).filter(Boolean).map((item) => option(item, indent + 8, depth, path));
  const args = array(node.args).filter(Boolean).map((item) => argument(item, indent + 8, depth, path));
  const extra = array(node.additionalSuggestions).map((item) => suggestion(item, indent + 8));
  if (typeof node.generateSpec === "function") stats.generate_specs += 1;
  return [
    ["description", some(node.description)],
    ["subcommands", list(subcommands, indent + 4)],
    ["options", list(options, indent + 4)],
    ["arguments", list(args, indent + 4)],
    ["additional_suggestions", list(extra, indent + 4)],
    ["requires_subcommand", flag(node.requiresSubcommand)],
    ["filter_strategy", FILTER[node.filterStrategy]],
    ["parser_directives", parserDirectives(node.parserDirectives, indent + 4, path)],
    ["has_generate_spec", flag(typeof node.generateSpec === "function")],
  ];
}

function subcommand(node, indent, depth, path) {
  stats.subcommands += 1;
  if (depth > MAX_DEPTH) { stats.truncated += 1; return record([["name", text(array(node.name)[0] ?? "")]], indent); }
  const names = array(node.name);
  return record([
    ["name", text(names[0] ?? "")],
    ["aliases", list(names.slice(1).map(text), indent + 4)],
    ...body(node, indent, depth, path),
    ...loadSpec(node.loadSpec, indent + 4, depth, path),
    ["presentation", presentation(node, indent + 4)],
  ], indent);
}

export function emitSpec(node, command) {
  stats.commands += 1;
  const names = array(node.name);
  return `${record([
    ["version", "1"],
    ["command", text(command)],
    ["aliases", list(names.filter((name) => name !== command).map(text), 4)],
    ...body(node, 0, 0, [command]),
    ...loadSpec(node.loadSpec, 4, 0, [command]),
    ["presentation", presentation(node, 4)],
  ], 0)}\n`;
}

export function emitPlaceholder(command) {
  stats.commands += 1;
  return `${record([["version", "1"], ["command", text(command)], ["placeholder", "true"]], 0)}\n`;
}
