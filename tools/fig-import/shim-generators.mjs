// The installed @fig/autocomplete-generators exports no `KeyValueSuggestions`,
// and Node cannot see the CommonJS module's named exports through the loader
// hook. Re-export the real helpers and stub the missing one.
import real from "@fig/autocomplete-generators/lib/index.js";

export const { filepaths, folders, ai, valueList, keyValue, keyValueList } = real;
export const KeyValueSuggestions = () => ({});
export default real;
