import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { EditorState, TransactionSpec } from '@codemirror/state';
import type { Text } from '@codemirror/state';
import { ask } from '@tauri-apps/plugin-dialog';
import { FileCode2, FileDiff, FolderTree, Ghost, History, Save, X } from 'lucide-react';
import { api, errorText, type DiffOf, type GitState } from '../api';
import { eolOf, fileState, languageFor, languageSlot } from '../editor';
import { MAX_LINES, fromDiff, fromText, type GhostLine } from '../ghost';
import { EditorPane, type DocStore } from './EditorPane';
import { FileTree } from './FileTree';
import { HistoryPane } from './HistoryPane';
import { DiffView } from './DiffView';
import { GhostView } from './GhostView';

/// Something to show, asked for from outside the code view.
export type Ask =
  | { kind: 'commit'; hash: string }
  | { kind: 'diff'; of: DiffOf; label: string }
  | { kind: 'file'; path: string };
/// An ask, numbered so the same one twice still opens twice.
export type Intent = Ask & { seq: number };

type Tab =
  | { kind: 'file'; id: string; path: string; dirty: boolean; readOnly: boolean; onDisk: 'same' | 'changed' | 'gone' }
  | { kind: 'diff'; id: string; path: string; of: DiffOf; label: string }
  | { kind: 'ghost'; id: string; path: string; label: string; lines: GhostLine[]; truncated: boolean };

interface Bench {
  tabs: Tab[];
  active: string | null;
  side: 'files' | 'history';
  scope: 'all' | 'file';
  openDirs: Set<string>;
}

/// What a file tab needs besides its editor state.
interface FileMeta {
  stamp: number;
  saved: Text;
  eol: '\n' | '\r\n';
}

// Kept per project while the window is open: leaving the code view, or
// switching sessions, comes back to the same tabs and unsaved edits.
const benches = new Map<string, Bench>();
const stores = new Map<string, DocStore & { meta: Map<string, FileMeta> }>();
const fileLists = new Map<string, string[]>();
// Ctrl+S in an editor state made by an earlier mount of the view still has
// to reach the one showing now.
const savers = new Map<string, (id: string) => void>();

const fileId = (path: string) => `f:${path}`;
const diffId = (of: DiffOf) => `d:${of.kind}:${of.kind === 'commit' ? of.hash : ''}:${of.path}`;
const basename = (p: string) => p.slice(p.lastIndexOf('/') + 1);

interface Props {
  /// The work tree's top, or the session's folder outside one.
  base: string;
  git: GitState | null;
  intent: Intent | null;
  onError: (text: string) => void;
  onNotify: (text: string) => void;
}

export function Workbench({ base, git, intent, onError, onNotify }: Props) {
  const [bench, setBenchState] = useState<Bench>(
    () => benches.get(base) ?? { tabs: [], active: null, side: 'files', scope: 'all', openDirs: new Set() },
  );
  const store = useMemo(() => {
    let s = stores.get(base);
    if (!s) {
      s = { states: new Map(), meta: new Map() };
      stores.set(base, s);
    }
    return s;
  }, [base]);
  const [files, setFiles] = useState<string[] | null>(() => fileLists.get(base) ?? null);
  const [reveal, setReveal] = useState<{ hash: string; seq: number } | null>(null);
  const filter = useRef<HTMLInputElement>(null);
  const apply = useRef<((id: string, spec: TransactionSpec) => void) | null>(null);
  const benchRef = useRef(bench);
  benchRef.current = bench;

  const setBench = useCallback(
    (f: (b: Bench) => Bench) =>
      setBenchState((b) => {
        const n = f(b);
        benches.set(base, n);
        benchRef.current = n;
        return n;
      }),
    [base],
  );
  const patchTab = useCallback(
    (id: string, patch: Partial<Tab>) =>
      setBench((b) => ({ ...b, tabs: b.tabs.map((t) => (t.id === id ? ({ ...t, ...patch } as Tab) : t)) })),
    [setBench],
  );

  const active = bench.tabs.find((t) => t.id === bench.active) ?? null;
  const changes = useMemo(() => new Map((git?.changes ?? []).map((c) => [c.path, c.code])), [git?.changes]);

  // ----- the file list
  const loadFiles = useCallback(() => {
    api
      .files(base)
      .then((f) => {
        fileLists.set(base, f);
        setFiles(f);
      })
      .catch(() => setFiles([]));
  }, [base]);
  useEffect(() => {
    if (!fileLists.has(base)) loadFiles();
  }, [base, loadFiles]);

  // ----- tabs
  const show = useCallback(
    (tab: Tab) =>
      setBench((b) => ({
        ...b,
        tabs: b.tabs.some((t) => t.id === tab.id) ? b.tabs : [...b.tabs, tab],
        active: tab.id,
      })),
    [setBench],
  );

  const save = useCallback(
    async (id: string, force = false) => {
      const tab = benchRef.current.tabs.find((t) => t.id === id);
      const st = store.states.get(id);
      const meta = store.meta.get(id);
      if (!tab || tab.kind !== 'file' || !st || !meta || tab.readOnly) return;
      const doc = st.doc;
      let text = doc.toString();
      if (meta.eol === '\r\n') text = text.replace(/\n/g, '\r\n');
      try {
        const stamp = await api.fileWrite(base, tab.path, text, force ? null : meta.stamp);
        store.meta.set(id, { ...meta, stamp, saved: doc });
        const now = store.states.get(id);
        patchTab(id, { dirty: !!now && !now.doc.eq(doc), onDisk: 'same' });
        onNotify(`Saved ${basename(tab.path)}`);
      } catch (e) {
        const msg = errorText(e);
        if (msg === 'changed on disk') {
          const yes = await ask(`${tab.path} changed on disk since it was opened — a session may have edited it.\n\nOverwrite it with your version?`, {
            title: 'File changed on disk',
            kind: 'warning',
            okLabel: 'Overwrite',
            cancelLabel: 'Keep it',
          });
          if (yes) await save(id, true);
          else patchTab(id, { onDisk: 'changed' });
        } else onError(msg);
      }
    },
    [base, store, patchTab, onError, onNotify],
  );
  savers.set(base, (id) => void save(id));

  /// Change an open document, whether the editor shows it or not.
  const applyTo = useCallback(
    (id: string, spec: TransactionSpec) => {
      if (apply.current) {
        apply.current(id, spec);
        return;
      }
      const st = store.states.get(id);
      if (st) store.states.set(id, st.update(spec).state);
    },
    [store],
  );

  const openFile = useCallback(
    async (path: string) => {
      const id = fileId(path);
      if (benchRef.current.tabs.some((t) => t.id === id) && store.states.has(id)) {
        setBench((b) => ({ ...b, active: id }));
        return;
      }
      try {
        const f = await api.fileRead(base, path);
        if (f.binary) {
          onError(`${path} is not a text file`);
          return;
        }
        const state = fileState(f.text, { readOnly: f.lossy, onSave: () => savers.get(base)?.(id) });
        store.states.set(id, state);
        store.meta.set(id, { stamp: f.stamp, saved: state.doc, eol: eolOf(f.text) });
        show({ kind: 'file', id, path, dirty: false, readOnly: f.lossy, onDisk: 'same' });
        if (f.lossy) onError(`${path} is not UTF-8 — opened read-only so saving cannot damage it`);
        const lang = await languageFor(path);
        if (lang) applyTo(id, { effects: languageSlot.reconfigure(lang) });
      } catch (e) {
        onError(errorText(e));
      }
    },
    [base, store, show, setBench, applyTo, onError],
  );

  const openDiff = useCallback(
    (of: DiffOf, label: string) => show({ kind: 'diff', id: diffId(of), path: of.path, of, label }),
    [show],
  );

  const openGhost = useCallback(
    (lines: GhostLine[], path: string, label: string) => {
      const typed = lines.filter((l) => l.kind === 'type' && l.text.trim()).length;
      if (!typed) {
        onError('Nothing to type there');
        return;
      }
      const truncated = lines.length > MAX_LINES;
      show({ kind: 'ghost', id: `g:${Date.now()}`, path, label, lines: truncated ? lines.slice(0, MAX_LINES) : lines, truncated });
    },
    [show, onError],
  );

  const ghostDiff = useCallback(
    async (of: DiffOf, label: string) => {
      try {
        openGhost(fromDiff(await api.diff(base, of)), of.path, label);
      } catch (e) {
        onError(errorText(e));
      }
    },
    [base, openGhost, onError],
  );

  /// The selection when there is one, else the whole file.
  const ghostFile = (tab: Extract<Tab, { kind: 'file' }>) => {
    const st = store.states.get(tab.id);
    if (!st) return;
    const sel = st.selection.main;
    if (sel.empty) {
      openGhost(fromText(st.doc.toString()), tab.path, tab.path);
      return;
    }
    const from = st.doc.lineAt(sel.from);
    const to = st.doc.lineAt(sel.to);
    openGhost(fromText(st.doc.sliceString(from.from, to.to), from.number), tab.path, `${tab.path}:${from.number}–${to.number}`);
  };

  const close = async (id: string) => {
    const tab = benchRef.current.tabs.find((t) => t.id === id);
    if (!tab) return;
    if (tab.kind === 'file' && tab.dirty) {
      const yes = await ask(`${tab.path} has unsaved changes. Close it and lose them?`, {
        title: 'Unsaved changes',
        kind: 'warning',
        okLabel: 'Discard',
        cancelLabel: 'Keep open',
      });
      if (!yes) return;
    }
    store.states.delete(id);
    store.meta.delete(id);
    setBench((b) => {
      const i = b.tabs.findIndex((t) => t.id === id);
      const tabs = b.tabs.filter((t) => t.id !== id);
      const next = b.active === id ? (tabs[Math.min(i, tabs.length - 1)]?.id ?? null) : b.active;
      return { ...b, tabs, active: next };
    });
  };

  const onDocChange = useCallback(
    (id: string, st: EditorState) => {
      const meta = store.meta.get(id);
      const dirty = !!meta && !st.doc.eq(meta.saved);
      const tab = benchRef.current.tabs.find((t) => t.id === id);
      if (tab && tab.kind === 'file' && tab.dirty !== dirty) patchTab(id, { dirty });
    },
    [store, patchTab],
  );

  /// Take what is on disk now, changing only the part that differs so the
  /// caret stays put where nothing changed.
  const reload = useCallback(
    async (id: string) => {
      const tab = benchRef.current.tabs.find((t) => t.id === id);
      if (!tab || tab.kind !== 'file') return;
      try {
        const f = await api.fileRead(base, tab.path);
        const st = store.states.get(id);
        const meta = store.meta.get(id);
        if (!st || !meta || f.binary) return;
        const next = f.text.replace(/\r\n?/g, '\n');
        const cur = st.doc.toString();
        let a = 0;
        while (a < cur.length && a < next.length && cur[a] === next[a]) a++;
        let b = 0;
        while (b < cur.length - a && b < next.length - a && cur[cur.length - 1 - b] === next[next.length - 1 - b]) b++;
        if (cur !== next) applyTo(id, { changes: { from: a, to: cur.length - b, insert: next.slice(a, next.length - b) } });
        const now = store.states.get(id) ?? st;
        store.meta.set(id, { stamp: f.stamp, saved: now.doc, eol: eolOf(f.text) });
        patchTab(id, { dirty: false, onDisk: 'same' });
      } catch (e) {
        onError(errorText(e));
      }
    },
    [base, store, applyTo, patchTab, onError],
  );

  // A session editing the open file: take its version when nothing here is
  // unsaved, say so when something is.
  const activeFile = active?.kind === 'file' ? active : null;
  useEffect(() => {
    if (!activeFile) return;
    const { id, path } = activeFile;
    const t = window.setInterval(async () => {
      if (document.hidden) return;
      const meta = store.meta.get(id);
      if (!meta) return;
      const stamp = await api.fileStamp(base, path).catch(() => meta.stamp);
      if (stamp === meta.stamp) return;
      const tab = benchRef.current.tabs.find((x) => x.id === id);
      if (!tab || tab.kind !== 'file') return;
      if (stamp === 0) {
        if (tab.onDisk !== 'gone') patchTab(id, { onDisk: 'gone' });
      } else if (!tab.dirty) void reload(id);
      else if (tab.onDisk !== 'changed') patchTab(id, { onDisk: 'changed' });
    }, 2000);
    return () => window.clearInterval(t);
  }, [activeFile?.id, base, store, reload, patchTab]); // eslint-disable-line react-hooks/exhaustive-deps

  // ----- asks from outside: the git panel, the palette
  useEffect(() => {
    if (!intent) return;
    if (intent.kind === 'commit') {
      setBench((b) => ({ ...b, side: 'history', scope: 'all' }));
      setReveal({ hash: intent.hash, seq: intent.seq });
    } else if (intent.kind === 'diff') openDiff(intent.of, intent.label);
    else void openFile(intent.path);
  }, [intent?.seq]); // eslint-disable-line react-hooks/exhaustive-deps

  // Ctrl+P: find a file. Ctrl+S outside the editor still saves the file
  // shown (inside it, the editor's own key has already done it).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = (e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey;
      if (mod && e.key.toLowerCase() === 's' && !e.defaultPrevented) {
        e.preventDefault();
        const b = benchRef.current;
        const tab = b.tabs.find((t) => t.id === b.active);
        if (tab?.kind === 'file') savers.get(base)?.(tab.id);
        return;
      }
      if (mod && e.key.toLowerCase() === 'p') {
        e.preventDefault();
        setBench((b) => ({ ...b, side: 'files' }));
        window.setTimeout(() => {
          filter.current?.focus();
          filter.current?.select();
        }, 0);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [setBench, base]);

  const toggleDir = (dir: string) =>
    setBench((b) => {
      const openDirs = new Set(b.openDirs);
      if (openDirs.has(dir)) openDirs.delete(dir);
      else openDirs.add(dir);
      return { ...b, openDirs };
    });

  const activePath = active?.path ?? null;

  return (
    <div className="bench">
      <aside className="bench-side">
        <div className="bench-side-tabs" role="tablist">
          <button type="button" role="tab" aria-selected={bench.side === 'files'} onClick={() => setBench((b) => ({ ...b, side: 'files' }))}>
            <FolderTree size={14} aria-hidden /> Files
          </button>
          <button type="button" role="tab" aria-selected={bench.side === 'history'} onClick={() => setBench((b) => ({ ...b, side: 'history' }))}>
            <History size={14} aria-hidden /> History
          </button>
        </div>
        {bench.side === 'files' ? (
          <FileTree
            files={files}
            changes={changes}
            active={activePath}
            open={bench.openDirs}
            onToggle={toggleDir}
            onOpen={(p) => void openFile(p)}
            onRefresh={loadFiles}
            filterRef={filter}
          />
        ) : git ? (
          <HistoryPane
            base={base}
            path={activePath}
            scope={bench.scope}
            onScope={(scope) => setBench((b) => ({ ...b, scope }))}
            changes={git.changes}
            reveal={reveal}
            onDiff={openDiff}
            onGhost={(of, label) => void ghostDiff(of, label)}
            onError={onError}
          />
        ) : (
          <div className="side-empty">Not a git repository</div>
        )}
      </aside>
      <section className="bench-main">
        {bench.tabs.length > 0 && (
          <div className="bench-tabs" role="tablist">
            {bench.tabs.map((t) => (
              <div key={t.id} className={`bench-tab ${t.id === bench.active ? 'active' : ''}`} role="tab" aria-selected={t.id === bench.active}>
                <button type="button" className="bench-tab-main" title={t.kind === 'file' ? t.path : t.label} onClick={() => setBench((b) => ({ ...b, active: t.id }))}>
                  {t.kind === 'file' ? <FileCode2 size={13} aria-hidden /> : t.kind === 'diff' ? <FileDiff size={13} aria-hidden /> : <Ghost size={13} aria-hidden />}
                  <span className="ellipsis">{basename(t.path)}</span>
                  {t.kind === 'diff' && t.of.kind === 'commit' && <span className="tiny faint mono">{t.of.hash.slice(0, 7)}</span>}
                </button>
                <button type="button" className="bench-tab-x" aria-label={`Close ${basename(t.path)}`} onClick={() => void close(t.id)}>
                  {t.kind === 'file' && t.dirty ? <span className="dirty-dot" aria-label="unsaved" /> : <X size={12} aria-hidden />}
                </button>
              </div>
            ))}
          </div>
        )}
        {active?.kind === 'file' && (
          <div className="bench-bar">
            <span className="mono small ellipsis" title={active.path}>{active.path}</span>
            {active.readOnly && <span className="chip small mist">read-only</span>}
            {active.onDisk === 'changed' && (
              <span className="bench-warn small">
                Changed on disk
                <button type="button" className="btn sm" onClick={() => void reload(active.id)}>Take theirs</button>
                <button type="button" className="btn sm" onClick={() => void save(active.id, true)}>Keep mine</button>
              </span>
            )}
            {active.onDisk === 'gone' && <span className="bench-warn small">Deleted on disk — saving writes it again</span>}
            <div style={{ flex: 1 }} />
            <button
              type="button"
              className="btn btn-icon-text sm"
              title="This file's commits"
              onClick={() => setBench((b) => ({ ...b, side: 'history', scope: 'file' }))}
              disabled={!git}
            >
              <History size={13} aria-hidden /> History
            </button>
            <button type="button" className="btn btn-icon-text sm" title="Ghost mode: retype the selection, or the whole file" onClick={() => ghostFile(active)}>
              <Ghost size={13} aria-hidden /> Ghost
            </button>
            <button type="button" className="btn btn-icon-text sm primary" disabled={!active.dirty || active.readOnly} onClick={() => void save(active.id)} title="Ctrl+S">
              <Save size={13} aria-hidden /> Save
            </button>
          </div>
        )}
        {active?.kind === 'diff' && (
          <div className="bench-bar">
            <span className="mono small ellipsis" title={active.label}>{active.label}</span>
            <div style={{ flex: 1 }} />
            <button type="button" className="btn btn-icon-text sm" onClick={() => void openFile(active.path)} title="Open the file as it is now">
              <FileCode2 size={13} aria-hidden /> Open file
            </button>
            <button
              type="button"
              className="btn btn-icon-text sm"
              disabled={!git}
              onClick={() => {
                setBench((b) => ({ ...b, side: 'history', scope: 'file' }));
              }}
            >
              <History size={13} aria-hidden /> File history
            </button>
            <button type="button" className="btn btn-icon-text sm" title="Ghost mode: retype what this change added" onClick={() => void ghostDiff(active.of, active.label)}>
              <Ghost size={13} aria-hidden /> Ghost
            </button>
          </div>
        )}
        <div className="bench-body">
          {active?.kind === 'file' && store.states.has(active.id) && (
            <EditorPane tabId={active.id} store={store} onChange={onDocChange} bind={(f) => (apply.current = f)} />
          )}
          {active?.kind === 'diff' && <DiffView key={active.id} base={base} of={active.of} />}
          {active?.kind === 'ghost' && (
            <GhostView key={active.id} id={active.id} lines={active.lines} label={active.label} truncated={active.truncated} onClose={() => void close(active.id)} />
          )}
          {!active && (
            <div className="bench-empty">
              <FileCode2 size={30} aria-hidden className="faint" />
              <p>Open a file from the list, or a commit under History to see exactly what it changed.</p>
              <p className="small mist">
                <Ghost size={13} aria-hidden style={{ verticalAlign: -2 }} /> Ghost mode: open a file or a diff and press Ghost — the code
                shows faint and you type it over, character by character.
              </p>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
