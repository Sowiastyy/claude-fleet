import { Fragment, useCallback, useEffect, useRef, useState } from 'react';
import { ChevronDown, ChevronRight, FileDiff, Ghost, RefreshCw } from 'lucide-react';
import { api, errorText, type CommitInfo, type DiffOf, type GitChange, type HistoryEntry } from '../api';
import { fmtAgo } from '../messages';
import { changeMark } from './FileTree';

const PAGE = 50;

interface Props {
  base: string;
  /// The file the shown tab is about, for "this file only".
  path: string | null;
  scope: 'all' | 'file';
  onScope: (s: 'all' | 'file') => void;
  /// Uncommitted changes, listed above the history.
  changes: GitChange[];
  /// A commit to open, from the git panel; `seq` says it is a new ask.
  reveal: { hash: string; seq: number } | null;
  onDiff: (of: DiffOf, label: string) => void;
  onGhost: (of: DiffOf, label: string) => void;
  onError: (text: string) => void;
}

// Commit details do not change; asked once per window.
const infos = new Map<string, CommitInfo>();
const asked = new Set<string>();

export function HistoryPane(p: Props) {
  const [entries, setEntries] = useState<HistoryEntry[] | null>(null);
  const [more, setMore] = useState(false);
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [, redraw] = useState(0);
  const list = useRef<HTMLDivElement>(null);
  const filePath = p.scope === 'file' ? p.path : null;
  const gen = useRef(0);

  const load = useCallback(
    async (skip: number) => {
      const mine = ++gen.current;
      try {
        const page = await api.history(p.base, filePath, skip, PAGE);
        if (mine !== gen.current) return;
        setEntries((e) => (skip === 0 ? page : [...(e ?? []), ...page]));
        setMore(page.length === PAGE);
      } catch (e) {
        if (mine === gen.current) {
          setEntries([]);
          p.onError(errorText(e));
        }
      }
    },
    [p.base, filePath], // eslint-disable-line react-hooks/exhaustive-deps
  );

  useEffect(() => {
    setEntries(null);
    void load(0);
  }, [load]);

  // Details for each commit opened, asked for once.
  useEffect(() => {
    for (const hash of open) {
      const key = `${p.base}|${hash}`;
      if (infos.has(key) || asked.has(key)) continue;
      asked.add(key);
      api
        .commitInfo(p.base, hash)
        .then((i) => {
          infos.set(key, i);
          redraw((n) => n + 1);
        })
        .catch((e) => p.onError(errorText(e)))
        .finally(() => asked.delete(key));
    }
  }, [open, p.base]); // eslint-disable-line react-hooks/exhaustive-deps

  const toggle = (hash: string) =>
    setOpen((o) => {
      const n = new Set(o);
      if (n.has(hash)) n.delete(hash);
      else n.add(hash);
      return n;
    });

  // From the git panel: show that commit, open.
  useEffect(() => {
    if (!p.reveal) return;
    const { hash } = p.reveal;
    setOpen((o) => new Set(o).add(hash));
    window.setTimeout(() => list.current?.querySelector(`[data-hash="${hash}"]`)?.scrollIntoView({ block: 'nearest' }), 80);
  }, [p.reveal]);

  const label = (e: HistoryEntry, path: string) => `${path} @ ${e.hash.slice(0, 7)}`;

  return (
    <div className="side-pane">
      <div className="side-scope">
        <div className="segmented sm" role="group" aria-label="History of">
          <button type="button" aria-pressed={p.scope === 'all'} onClick={() => p.onScope('all')}>
            All commits
          </button>
          <button
            type="button"
            aria-pressed={p.scope === 'file'}
            disabled={!p.path}
            title={p.path ? `Only the commits that touched ${p.path}` : 'Open a file first'}
            onClick={() => p.onScope('file')}
          >
            This file
          </button>
        </div>
        <button type="button" className="icon-btn sm" aria-label="Refresh history" title="Read the history again" onClick={() => void load(0)}>
          <RefreshCw size={13} aria-hidden />
        </button>
      </div>
      {filePath && <div className="side-note ellipsis" title={filePath}>{filePath}</div>}
      <div className="side-list" ref={list}>
        {p.scope === 'all' && p.changes.length > 0 && (
          <>
            <div className="side-heading">Uncommitted</div>
            {p.changes.map((c) => {
              const m = changeMark(c.code);
              const of: DiffOf = { kind: 'worktree', path: c.path, untracked: c.code === '??' };
              return (
                <div className="hist-file" key={c.path}>
                  <button type="button" className="hist-file-main" title={c.path} onClick={() => p.onDiff(of, `${c.path} (uncommitted)`)}>
                    <span className="tree-mark">{m}</span>
                    <span className="ellipsis">{c.path}</span>
                    {c.added != null && <span className="plus">+{c.added}</span>}
                    {!!c.removed && <span className="minus">−{c.removed}</span>}
                  </button>
                  <button type="button" className="icon-btn sm" title="Ghost mode: retype what changed" aria-label="Ghost mode" onClick={() => p.onGhost(of, c.path)}>
                    <Ghost size={13} aria-hidden />
                  </button>
                </div>
              );
            })}
            <div className="side-heading">History</div>
          </>
        )}
        {entries === null && <div className="side-empty">Reading history…</div>}
        {entries?.length === 0 && <div className="side-empty">{filePath ? 'No commits touched this file' : 'No commits yet'}</div>}
        {entries?.map((e) => {
          const isOpen = open.has(e.hash);
          const i = isOpen ? infos.get(`${p.base}|${e.hash}`) : undefined;
          const path = e.path;
          return (
            <Fragment key={e.hash}>
              <div className={`hist-commit ${isOpen ? 'open' : ''}`} data-hash={e.hash}>
                <button type="button" className="hist-commit-main" onClick={() => toggle(e.hash)}>
                  {isOpen ? <ChevronDown size={13} aria-hidden /> : <ChevronRight size={13} aria-hidden />}
                  <span className="stack">
                    <span className="ellipsis">{e.subject}</span>
                    <span className="tiny mist ellipsis">
                      <span className="mono">{e.hash.slice(0, 7)}</span> · {e.author} · {fmtAgo(e.agoSecs)}
                    </span>
                  </span>
                </button>
                {path && (
                  <button
                    type="button"
                    className="icon-btn sm"
                    title={`What this commit did to ${path}`}
                    aria-label="Show the diff"
                    onClick={() => p.onDiff({ kind: 'commit', hash: e.hash, path }, label(e, path))}
                  >
                    <FileDiff size={13} aria-hidden />
                  </button>
                )}
              </div>
              {isOpen && (
                <div className="hist-detail">
                  {!i ? (
                    <div className="side-empty">…</div>
                  ) : (
                    <>
                      <div className="tiny mist">
                        {i.author} &lt;{i.email}&gt; · {i.date}
                      </div>
                      {i.message.includes('\n') && <pre className="hist-message">{i.message.slice(i.message.indexOf('\n')).trim()}</pre>}
                      {i.files.map((f) => {
                        const of: DiffOf = { kind: 'commit', hash: e.hash, path: f.path };
                        return (
                          <div className={`hist-file ${f.path === path ? 'this' : ''}`} key={f.path}>
                            <button type="button" className="hist-file-main" title={f.path} onClick={() => p.onDiff(of, label(e, f.path))}>
                              <span className="ellipsis">{f.path}</span>
                              {f.added == null ? <span className="faint">bin</span> : <span className="plus">+{f.added}</span>}
                              {!!f.removed && <span className="minus">−{f.removed}</span>}
                            </button>
                            <button
                              type="button"
                              className="icon-btn sm"
                              title="Ghost mode: retype what this commit added"
                              aria-label="Ghost mode"
                              disabled={f.added == null || f.added === 0}
                              onClick={() => p.onGhost(of, label(e, f.path))}
                            >
                              <Ghost size={13} aria-hidden />
                            </button>
                          </div>
                        );
                      })}
                    </>
                  )}
                </div>
              )}
            </Fragment>
          );
        })}
        {more && (
          <button type="button" className="dashed side-more" onClick={() => void load(entries?.length ?? 0)}>
            Load older commits
          </button>
        )}
      </div>
    </div>
  );
}
