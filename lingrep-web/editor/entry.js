// Re-exports the CodeMirror modules the lingrep patterns editor uses. Bundled by esbuild
// into a single ESM file (see package.json "build") so the browser loads one
// vendored asset with no CDN and no module-graph waterfall.
export { EditorState, StateField } from "@codemirror/state";
export {
  EditorView,
  ViewPlugin,
  Decoration,
  keymap,
  placeholder,
  hoverTooltip,
  showTooltip,
} from "@codemirror/view";
export {
  defaultKeymap,
  history,
  historyKeymap,
  indentWithTab,
} from "@codemirror/commands";
export { bracketMatching } from "@codemirror/language";
export {
  autocompletion,
  closeBrackets,
  closeBracketsKeymap,
  completionKeymap,
  snippetCompletion,
} from "@codemirror/autocomplete";
export {
  forceLinting,
  linter,
  lintGutter,
  lintKeymap,
  previousDiagnostic,
} from "@codemirror/lint";
export { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
