// Re-export the CommonJS helpers through ESM and tag the path generators whose
// behavior Wisp can express exactly as a native RON template.
import real from "@fig/autocomplete-generators/lib/index.js";

function tagged(generator, template) {
  Object.defineProperty(generator, "__wispTemplate", { value: template });
  return generator;
}

export function filepaths(options) {
  const generator = real.filepaths(options);
  const keys = Object.keys(options ?? {});
  if (keys.length === 0 || (keys.length === 1 && options.showFolders === "always")) {
    return tagged(generator, "filepaths");
  }
  if (keys.length === 1 && options.showFolders === "only") {
    return tagged(generator, "folders");
  }
  return generator;
}
Object.assign(filepaths, real.filepaths);
tagged(filepaths, "filepaths");

export function folders() {
  return tagged(real.folders(), "folders");
}
Object.assign(folders, real.folders);
tagged(folders, "folders");

export const { ai, valueList, keyValue, keyValueList } = real;
export const KeyValueSuggestions = () => ({});
export default { ...real, filepaths, folders };
