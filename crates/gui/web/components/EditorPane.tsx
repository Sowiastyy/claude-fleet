import { useEffect, useRef } from 'react';
import type { EditorState, TransactionSpec } from '@codemirror/state';
import { EditorView } from '@codemirror/view';

/// Where a file's editor state lives while its tab is not the one shown.
export interface DocStore {
  states: Map<string, EditorState>;
}

/// The one editor the code view has. Switching tabs swaps the state it
/// shows, so each file keeps its own undo history, selection and scroll.
export function EditorPane({
  tabId,
  store,
  onChange,
  bind,
}: {
  tabId: string;
  store: DocStore;
  /// The document of `id` changed.
  onChange: (id: string, state: EditorState) => void;
  /// Hands out a way to change any open document, shown or not.
  bind: (apply: ((id: string, spec: TransactionSpec) => void) | null) => void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const shown = useRef(tabId);
  const change = useRef(onChange);
  change.current = onChange;

  useEffect(() => {
    const v = new EditorView({
      parent: host.current!,
      state: store.states.get(shown.current),
      dispatchTransactions: (trs, v) => {
        v.update(trs);
        store.states.set(shown.current, v.state);
        if (trs.some((tr) => tr.docChanged)) change.current(shown.current, v.state);
      },
    });
    view.current = v;
    bind((id, spec) => {
      if (id === shown.current) {
        v.dispatch(spec);
        return;
      }
      const st = store.states.get(id);
      if (!st) return;
      const tr = st.update(spec);
      store.states.set(id, tr.state);
      if (tr.docChanged) change.current(id, tr.state);
    });
    v.focus();
    return () => {
      bind(null);
      store.states.set(shown.current, v.state);
      v.destroy();
      view.current = null;
    };
  }, [store]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const v = view.current;
    if (!v || shown.current === tabId) return;
    store.states.set(shown.current, v.state);
    shown.current = tabId;
    const st = store.states.get(tabId);
    if (st) {
      v.setState(st);
      // A new state starts at the top; go back to where the caret was.
      v.dispatch({ effects: EditorView.scrollIntoView(st.selection.main.head, { y: 'center' }) });
    }
    v.focus();
  }, [tabId, store]);

  return <div className="editor-host" ref={host} />;
}
