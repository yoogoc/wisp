// Declarative equivalents of Fig's JavaScript generator callbacks. This is
// importer-only metadata: the emitted command RON owns the runtime behavior.

const rule = (script, pipeline, kind, rejectPrefixes = []) => ({
  script,
  pipeline,
  kind,
  rejectPrefixes,
});

const RULES = [
  rule(["git", "--no-optional-locks", "branch", "--no-color", "--sort=-committerdate"], 'Lines((strip_prefix: Some("* "), reject_containing: ["HEAD detached"]))', "Branch"),
  rule(["git", "--no-optional-locks", "branch", "-a", "--no-color", "--sort=-committerdate"], 'Lines((strip_prefix: Some("* "), reject_containing: ["HEAD detached"]))', "Branch"),
  rule(["git", "--no-optional-locks", "branch", "-r", "--no-color", "--sort=-committerdate"], 'Lines((strip_prefix: Some("* "), reject_containing: ["HEAD detached"]))', "Branch"),
  rule(["git", "branch", "--no-color"], 'Lines((strip_prefix: Some("* ")))', "Branch"),
  rule(["git", "remote"], undefined, "Branch"),
  rule(["git", "--no-optional-locks", "remote", "-v"], 'Lines((delimiter: Some(""), name: 0, description: Some(At(1))))', "Branch"),
  rule(["git", "--no-optional-locks", "log", "--oneline"], 'Lines((delimiter: Some(""), name: 0, description: Some(From(1))))'),
  rule(["git", "rev-list", "--all", "--oneline"], 'Lines((delimiter: Some(""), name: 0, description: Some(From(1))))'),
  rule(["git", "--no-optional-locks", "status", "--short"], 'Lines((delimiter: Some(""), name: 1))', "File"),
  rule(["git", "--no-optional-locks", "diff", "--cached", "--name-only"], undefined, "File"),
  rule(["git", "--no-optional-locks", "tag", "--list", "--sort=-committerdate"], undefined, "Branch"),
  rule(["git", "--no-optional-locks", "stash", "list"], 'Lines((delimiter: Some(":"), name: 2, description: Some(At(1)), insert: Some(At(0))))'),
  rule(["git", "--no-optional-locks", "config", "--get-regexp", "^alias."], 'Lines((strip_prefix: Some("alias."), delimiter: Some(""), name: 0, description: Some(From(1))))'),
  rule(["git", "config", "--get-regexp", ".*"], 'Lines((delimiter: Some(""), name: 0))'),
  rule(["docker", "ps", "--format", "{{ json . }}"], 'Json((name: Some("Names"), description: Some("Image")))'),
  rule(["podman", "ps", "--format", "{{ json . }}"], 'Json((name: Some("Names"), description: Some("Image")))'),
  rule(["docker", "image", "ls", "--format", "{{ json . }}"], 'Json((name: Some("ID"), description: Some("Repository")))'),
  rule(["docker", "images", "--format", "{{.Repository}} {{.Size}} {{.Tag}} {{.ID}}"], 'Lines((delimiter: Some(""), name: 0, description: Some(From(1))))'),
  rule(["podman", "images", "--format", "{{.Repository}} {{.Size}} {{.Tag}} {{.ID}}"], 'Lines((delimiter: Some(""), name: 0, description: Some(From(1))))'),
  rule(["docker", "context", "list", "--format", "{{ json . }}"], 'Json((name: Some("Name"), description: Some("Description")))'),
  rule(["docker", "service", "list", "--format", "{{ json . }}"], 'Json((name: Some("Name"), description: Some("Image")))'),
  rule(["docker", "node", "list", "--format", "{{ json . }}"], 'Json((name: Some("ID"), description: Some("Hostname")))'),
  rule(["kubectl", "get", "namespaces"], 'Lines((skip: 1, delimiter: Some(""), name: 0))'),
  rule(["cargo", "metadata"], 'Json((path: ["packages"], name: Some("name"), description: Some("version")))'),
  rule(["cargo", "read-manifest"], 'Json((path: ["features"], keys: true))'),
  rule(["brew", "list", "-1"], 'Lines((reject_containing: ["="]))'),
  rule(["pnpm", "ls"], 'Lines((skip: 3, delimiter: Some(""), name: 0, description: Some(From(1)), reject_containing: ["dependencies", "workspace:"]))'),
  rule(["gh", "pr", "list"], 'Json((name: Some("number"), description: Some("title")))'),
  rule(["gh", "alias", "list"], 'Lines((delimiter: Some(":"), name: 0, description: Some(From(1))))'),
  rule(["tmux", "ls"], 'Lines((delimiter: Some(":"), name: 0, description: Some(From(1))))'),
  rule(["tmux", "lsw"], 'Lines((delimiter: Some(":"), name: 0, description: Some(From(1))))'),
  rule(["tmux", "lsp"], 'Lines((delimiter: Some(":"), name: 0, description: Some(From(1))))'),
  rule(["tmux", "lsb"], 'Lines((delimiter: Some(":"), name: 0, description: Some(From(1))))'),
  rule(["tmux", "lsc"], 'Lines((delimiter: Some(":"), name: 0, description: Some(From(1))))'),
];

export function generatorMetadata(script) {
  return RULES
    .filter((candidate) => candidate.script.every((word, index) => script[index] === word))
    .sort((left, right) => right.script.length - left.script.length)[0];
}

const NATIVES = new Map([
  ["npm run", "PackageJsonScripts"],
  ["npm run-script", "PackageJsonScripts"],
  ["pnpm run", "PackageJsonScripts"],
  ["yarn run", "PackageJsonScripts"],
  ["make", "MakeTargets"],
  ["ssh", "SshHosts"],
  ["scp", "SshHosts"],
  ["sftp", "SshHosts"],
]);

export const nativeForPath = (path) => NATIVES.get(path.join(" "));
