// CodeMirror, set up for the code view: the fleet's colors (read from the
// theme's CSS variables, so a theme switch needs nothing here), the language
// a file's name says, and Ctrl+S.
import { basicSetup } from 'codemirror';
import { Compartment, EditorState, type Extension } from '@codemirror/state';
import { EditorView, keymap } from '@codemirror/view';
import { indentWithTab } from '@codemirror/commands';
import { HighlightStyle, LanguageDescription, syntaxHighlighting } from '@codemirror/language';
import { languages } from '@codemirror/language-data';
import { tags as t } from '@lezer/highlight';

const theme = EditorView.theme({
  '&': { height: '100%', color: 'var(--fog)', backgroundColor: 'transparent', fontSize: '13px' },
  '.cm-scroller': { fontFamily: 'var(--mono)', lineHeight: '1.55' },
  '.cm-content': { caretColor: 'var(--accent)', padding: '10px 0' },
  '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--accent)', borderLeftWidth: '2px' },
  '&.cm-focused': { outline: 'none' },
  '.cm-gutters': { backgroundColor: 'transparent', color: 'var(--faint)', border: 'none', paddingLeft: '6px' },
  '.cm-activeLineGutter': { backgroundColor: 'transparent', color: 'var(--fog-2)' },
  '.cm-activeLine': { backgroundColor: 'color-mix(in srgb, var(--fog) 4%, transparent)' },
  '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection': {
    backgroundColor: 'color-mix(in srgb, var(--accent) 28%, transparent) !important',
  },
  '.cm-selectionMatch': { backgroundColor: 'color-mix(in srgb, var(--accent) 16%, transparent)' },
  '.cm-matchingBracket, &.cm-focused .cm-matchingBracket': {
    backgroundColor: 'color-mix(in srgb, var(--accent) 22%, transparent)',
    outline: 'none',
  },
  '.cm-foldPlaceholder': { backgroundColor: 'var(--deck-2)', border: '1px solid var(--rope-2)', color: 'var(--mist)' },
  '.cm-panels': { backgroundColor: 'var(--hull)', color: 'var(--fog)' },
  '.cm-panels.cm-panels-top': { borderBottom: '1px solid var(--line)' },
  '.cm-panels.cm-panels-bottom': { borderTop: '1px solid var(--line)' },
  '.cm-panel input, .cm-panel button, .cm-textfield': {
    backgroundColor: 'var(--ink)',
    color: 'var(--fog)',
    border: '1px solid var(--rope-2)',
    borderRadius: '6px',
  },
  '.cm-button': { backgroundImage: 'none', backgroundColor: 'var(--deck)' },
  '.cm-searchMatch': { backgroundColor: 'color-mix(in srgb, var(--warn) 30%, transparent)' },
  '.cm-searchMatch.cm-searchMatch-selected': { backgroundColor: 'color-mix(in srgb, var(--accent) 45%, transparent)' },
  '.cm-tooltip': { backgroundColor: 'var(--deck)', border: '1px solid var(--rope-2)', color: 'var(--fog)' },
  '.cm-tooltip-autocomplete > ul > li[aria-selected]': { backgroundColor: 'var(--rope)', color: 'var(--fog)' },
});

const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.controlKeyword, t.moduleKeyword, t.operatorKeyword, t.modifier], color: 'var(--syn-keyword)' },
  { tag: [t.string, t.special(t.string), t.regexp, t.character], color: 'var(--syn-string)' },
  { tag: [t.comment, t.lineComment, t.blockComment, t.docComment], color: 'var(--syn-comment)', fontStyle: 'italic' },
  { tag: [t.number, t.bool, t.null, t.atom], color: 'var(--syn-number)' },
  { tag: [t.typeName, t.className, t.namespace, t.standard(t.typeName)], color: 'var(--syn-type)' },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.macroName], color: 'var(--syn-fn)' },
  { tag: [t.propertyName, t.attributeName], color: 'var(--syn-prop)' },
  { tag: [t.tagName, t.angleBracket], color: 'var(--syn-tag)' },
  { tag: [t.meta, t.annotation, t.processingInstruction], color: 'var(--syn-meta)' },
  { tag: [t.heading, t.strong], color: 'var(--syn-keyword)', fontWeight: '600' },
  { tag: t.emphasis, fontStyle: 'italic' },
  { tag: t.link, color: 'var(--syn-string)', textDecoration: 'underline' },
  { tag: t.invalid, color: 'var(--danger)' },
]);

export interface FileSetup {
  readOnly: boolean;
  onSave: () => void;
}

/// Each file keeps its language in one of these, filled once it has loaded.
export const languageSlot = new Compartment();

/// The editor works in `\n`; a file written mostly with `\r\n` gets it back
/// on save.
export function eolOf(text: string): '\n' | '\r\n' {
  const crlf = (text.match(/\r\n/g) ?? []).length;
  const lf = (text.match(/\n/g) ?? []).length - crlf;
  return crlf > lf ? '\r\n' : '\n';
}

export function fileState(text: string, setup: FileSetup): EditorState {
  const extensions: Extension[] = [
    basicSetup,
    keymap.of([
      indentWithTab,
      {
        key: 'Mod-s',
        preventDefault: true,
        run: () => {
          setup.onSave();
          return true;
        },
      },
    ]),
    theme,
    syntaxHighlighting(highlight),
    languageSlot.of([]),
    EditorState.readOnly.of(setup.readOnly),
  ];
  return EditorState.create({ doc: text, extensions });
}

/// The language a file's name says, loaded on first use; `null` for plain text.
export async function languageFor(path: string): Promise<Extension | null> {
  const name = path.split('/').pop() ?? path;
  const desc = LanguageDescription.matchFilename(languages, name);
  if (!desc) return null;
  try {
    return await desc.load();
  } catch {
    return null;
  }
}
