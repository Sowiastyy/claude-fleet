import { memo, useMemo, useState, type RefObject } from 'react';
import { ChevronDown, ChevronRight, File, Folder, RefreshCw, Search } from 'lucide-react';

const CODE_COLOR: Record<string, string> = {
  M: 'var(--warn)',
  A: 'var(--idle)',
  D: 'var(--danger)',
  R: 'var(--waiting)',
  U: 'var(--idle)',
};

/// The letter the git panel shows for a porcelain code.
export function changeMark(code: string): string {
  if (code === '??') return 'U';
  return code.trim()[0] ?? '?';
}

interface Dir {
  name: string;
  path: string;
  dirs: Dir[];
  files: string[];
}

function tree(paths: string[]): Dir {
  const root: Dir = { name: '', path: '', dirs: [], files: [] };
  const index = new Map<string, Dir>([['', root]]);
  for (const p of paths) {
    const parts = p.split('/');
    let dir = root;
    for (let i = 0; i < parts.length - 1; i++) {
      const path = parts.slice(0, i + 1).join('/');
      let next = index.get(path);
      if (!next) {
        next = { name: parts[i], path, dirs: [], files: [] };
        index.set(path, next);
        dir.dirs.push(next);
      }
      dir = next;
    }
    dir.files.push(p);
  }
  const sort = (d: Dir) => {
    d.dirs.sort((a, b) => a.name.localeCompare(b.name));
    d.files.sort((a, b) => a.localeCompare(b));
    d.dirs.forEach(sort);
  };
  sort(root);
  return root;
}

/// Best first: the name containing the query, then the path, then the
/// query's letters in order anywhere in the path.
function search(paths: string[], q: string): string[] {
  const needle = q.toLowerCase().replace(/\\/g, '/');
  const scored: Array<[number, string]> = [];
  for (const p of paths) {
    const lower = p.toLowerCase();
    const name = lower.slice(lower.lastIndexOf('/') + 1);
    let score = -1;
    if (name.startsWith(needle)) score = 0;
    else if (name.includes(needle)) score = 1;
    else if (lower.includes(needle)) score = 2;
    else {
      let i = 0;
      for (const ch of lower) if (ch === needle[i]) i++;
      if (i === needle.length) score = 3;
    }
    if (score >= 0) scored.push([score * 10000 + p.length, p]);
    if (scored.length > 5000) break;
  }
  return scored.sort((a, b) => a[0] - b[0]).slice(0, 300).map(([, p]) => p);
}

const basename = (p: string) => p.slice(p.lastIndexOf('/') + 1);

interface Props {
  files: string[] | null;
  changes: Map<string, string>;
  active: string | null;
  open: Set<string>;
  onToggle: (dir: string) => void;
  onOpen: (path: string) => void;
  onRefresh: () => void;
  filterRef: RefObject<HTMLInputElement | null>;
}

export function FileTree({ files, changes, active, open, onToggle, onOpen, onRefresh, filterRef }: Props) {
  const [query, setQuery] = useState('');
  const root = useMemo(() => tree(files ?? []), [files]);
  const found = useMemo(() => (query.trim() ? search(files ?? [], query.trim()) : null), [files, query]);
  // A folder holding a change shows it, so a change deep down can be found.
  const changedDirs = useMemo(() => {
    const s = new Set<string>();
    for (const p of changes.keys()) {
      const parts = p.split('/');
      for (let i = 1; i < parts.length; i++) s.add(parts.slice(0, i).join('/'));
    }
    return s;
  }, [changes]);

  return (
    <div className="side-pane">
      <div className="side-search">
        <Search size={14} aria-hidden className="faint" />
        <input
          ref={filterRef}
          value={query}
          placeholder="Find a file (Ctrl+P)"
          spellCheck={false}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && found?.length) onOpen(found[0]);
            if (e.key === 'Escape') setQuery('');
          }}
        />
        <button type="button" className="icon-btn sm" title="Read the file list again" aria-label="Refresh files" onClick={onRefresh}>
          <RefreshCw size={13} aria-hidden />
        </button>
      </div>
      <div className="side-list" role="tree">
        {files === null && <div className="side-empty">Reading files…</div>}
        {files?.length === 0 && <div className="side-empty">No files here</div>}
        {found
          ? found.map((p) => (
              <FileRow key={p} path={p} label={basename(p)} hint={p} depth={0} code={changes.get(p)} active={p === active} onOpen={onOpen} />
            ))
          : files && (
              <DirRows
                dir={root}
                depth={0}
                open={open}
                onToggle={onToggle}
                onOpen={onOpen}
                changes={changes}
                changedDirs={changedDirs}
                active={active}
              />
            )}
        {found?.length === 0 && <div className="side-empty">Nothing matches</div>}
      </div>
    </div>
  );
}

function DirRows(p: {
  dir: Dir;
  depth: number;
  open: Set<string>;
  onToggle: (dir: string) => void;
  onOpen: (path: string) => void;
  changes: Map<string, string>;
  changedDirs: Set<string>;
  active: string | null;
}) {
  return (
    <>
      {p.dir.dirs.map((d) => {
        const isOpen = p.open.has(d.path);
        return (
          <div key={d.path} role="treeitem" aria-expanded={isOpen}>
            <button
              type="button"
              className="tree-row"
              style={{ paddingLeft: 10 + p.depth * 14 }}
              onClick={() => p.onToggle(d.path)}
            >
              {isOpen ? <ChevronDown size={13} aria-hidden /> : <ChevronRight size={13} aria-hidden />}
              <Folder size={13} aria-hidden className="faint" />
              <span className="ellipsis" style={{ color: p.changedDirs.has(d.path) ? 'var(--warn)' : undefined }}>{d.name}</span>
            </button>
            {isOpen && <DirRows {...p} dir={d} depth={p.depth + 1} />}
          </div>
        );
      })}
      {p.dir.files.map((f) => (
        <FileRow key={f} path={f} label={basename(f)} depth={p.depth} code={p.changes.get(f)} active={f === p.active} onOpen={p.onOpen} />
      ))}
    </>
  );
}

const FileRow = memo(function FileRow(p: {
  path: string;
  label: string;
  hint?: string;
  depth: number;
  code: string | undefined;
  active: boolean;
  onOpen: (path: string) => void;
}) {
  const m = p.code ? changeMark(p.code) : null;
  return (
    <button
      type="button"
      role="treeitem"
      className={`tree-row file ${p.active ? 'active' : ''}`}
      style={{ paddingLeft: 10 + p.depth * 14 + 13 }}
      title={p.path}
      onClick={() => p.onOpen(p.path)}
    >
      <File size={13} aria-hidden className="faint" />
      <span className="stack">
        <span className="ellipsis" style={{ color: m ? CODE_COLOR[m] : undefined }}>{p.label}</span>
        {p.hint && p.hint !== p.label && <span className="ellipsis tiny faint">{p.hint}</span>}
      </span>
      {m && (
        <span className="tree-mark" style={{ color: CODE_COLOR[m] ?? 'var(--mist)' }}>
          {m}
        </span>
      )}
    </button>
  );
});
