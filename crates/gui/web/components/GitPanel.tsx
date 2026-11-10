import { useState, type CSSProperties } from 'react';
import { ArrowDownToLine, ArrowUpFromLine, GitBranch, GitCommitHorizontal, PanelRightClose, Sparkles } from 'lucide-react';
import { api, errorText, type GitChange, type GitState } from '../api';
import { fmtAgo } from '../messages';

// Porcelain codes as one marker: what happened to the file, in VS Code's letters.
const MARKS: Record<string, { label: string; color: string }> = {
  M: { label: 'Modified', color: 'var(--warn)' },
  A: { label: 'Added', color: 'var(--idle)' },
  D: { label: 'Deleted', color: 'var(--danger)' },
  R: { label: 'Renamed', color: 'var(--waiting)' },
  C: { label: 'Copied', color: 'var(--waiting)' },
  U: { label: 'Untracked', color: 'var(--idle)' },
  '!': { label: 'Conflict', color: 'var(--danger)' },
};

function mark(code: string): string {
  if (code === '??') return 'U';
  if (code.includes('U') || code === 'AA' || code === 'DD') return '!';
  const c = code.trim()[0] ?? '?';
  return c === 'T' ? 'M' : c;
}

/// `a/b/c.ts` as its name and the folder it sits in.
function split(path: string): [string, string] {
  const p = path.includes(' -> ') ? path.split(' -> ').pop()! : path;
  const i = p.replace(/\/$/, '').lastIndexOf('/');
  return i < 0 ? [p, ''] : [p.slice(i + 1), p.slice(0, i)];
}

interface Props {
  cwd: string;
  git: GitState | null;
  onRefresh: () => void;
  onToast: (text: string, bad?: boolean) => void;
  /// Open in the code view: a commit's files, or an uncommitted file's diff.
  onOpenCommit: (hash: string) => void;
  onOpenChange: (change: GitChange) => void;
  onClose: () => void;
}

export function GitPanel({ cwd, git, onRefresh, onToast, onOpenCommit, onOpenChange, onClose }: Props) {
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState<string | null>(null);

  const run = async (what: string, f: () => Promise<unknown>, done: string) => {
    setBusy(what);
    try {
      await f();
      onToast(done);
    } catch (e) {
      onToast(errorText(e), true);
    } finally {
      setBusy(null);
      onRefresh();
    }
  };

  const hide = (
    <button type="button" className="icon-btn git-hide" aria-label="Hide source control" title="Hide" onClick={onClose}>
      <PanelRightClose size={16} aria-hidden />
    </button>
  );

  if (!git) {
    return (
      <aside className="git" aria-label="Git">
        <div className="git-head">
          <div className="git-branch">
            <span className="mist" style={{ flex: 1, fontWeight: 400 }}>Not a git repository</span>
            {hide}
          </div>
        </div>
      </aside>
    );
  }

  const ahead = git.ahead;
  return (
    <aside className="git" aria-label="Git">
      <div className="git-head">
        <div className="git-branch">
          <GitBranch size={15} color="var(--accent)" aria-hidden />
          <span className="ellipsis" style={{ flex: 1 }}>{git.branch ?? 'detached HEAD'}</span>
          {hide}
        </div>
        <span className="small mist">
          {git.upstream ? `${ahead} ahead, ${git.behind} behind ${git.upstream}` : 'no upstream'}
        </span>
      </div>
      <div className="git-box">
        <label htmlFor="commit-msg" className="small" style={{ fontWeight: 600, color: 'var(--fog-2)' }}>Commit message</label>
        <textarea id="commit-msg" rows={3} value={message} onChange={(e) => setMessage(e.target.value)} placeholder="Empty: Haiku writes it" />
        <div className="git-buttons">
          <button
            type="button"
            className="btn primary"
            disabled={!!busy || !git.changes.length}
            onClick={() =>
              run('commit', async () => {
                const hash = await api.commit(cwd, message);
                setMessage('');
                return hash;
              }, 'Committed')
            }
          >
            <GitCommitHorizontal size={14} aria-hidden />
            {busy === 'commit' ? '…' : 'Commit'}
          </button>
          <button type="button" className="btn" disabled={!!busy} onClick={() => run('push', () => api.push(cwd), 'Pushed')}>
            <ArrowUpFromLine size={14} aria-hidden />
            {busy === 'push' ? '…' : 'Push'}
          </button>
          <button type="button" className="btn" disabled={!!busy} onClick={() => run('pull', () => api.pull(cwd), 'Pulled')}>
            <ArrowDownToLine size={14} aria-hidden />
            {busy === 'pull' ? '…' : 'Pull'}
          </button>
        </div>
        <button
          type="button"
          className="dashed"
          disabled={!!busy || !git.changes.length}
          onClick={() =>
            run('generate', async () => setMessage(await api.generate(cwd)), 'Message written')
          }
        >
          <Sparkles size={14} aria-hidden />
          {busy === 'generate' ? 'Writing…' : 'Generate message'}
        </button>
      </div>
      <div className="git-sub">
        <h3>Changes</h3>
        <span className="small faint">
          {git.changesTotal} <span style={{ color: 'var(--idle)' }}>+{git.added}</span> <span style={{ color: 'var(--danger)' }}>−{git.removed}</span>
        </span>
      </div>
      {git.changes.map((c) => {
        const m = mark(c.code);
        const info = MARKS[m] ?? { label: 'Changed', color: 'var(--mist)' };
        const [name, dir] = split(c.path);
        return (
          <button
            type="button"
            className={`change ${m === 'D' ? 'gone' : ''}`}
            key={c.path}
            title={`${c.path}, ${info.label.toLowerCase()}`}
            style={{ '--mark': info.color } as CSSProperties}
            onClick={() => onOpenChange(c)}
          >
            <span className="mark" aria-label={info.label}>{m}</span>
            <span className="change-file">
              <span className="change-name">{name}</span>
              {dir && <span className="change-dir">{dir}</span>}
            </span>
            <span className="change-stat">
              {!!c.added && <span className="plus">+{c.added}</span>}
              {!!c.removed && <span className="minus">−{c.removed}</span>}
            </span>
          </button>
        );
      })}
      <div className="git-sub">
        <h3>History</h3>
      </div>
      {git.log.map((k, i) => (
        <button type="button" className="commit" key={k.hash} title="Show what this commit changed" onClick={() => onOpenCommit(k.hash)}>
          <span className={`dot ${i < ahead ? 'ahead' : ''}`} aria-hidden />
          <span className="stack">
            <span className="ellipsis">{k.subject}</span>
            <span className="small" style={{ color: i < ahead ? 'var(--warn)' : 'var(--mist)' }}>
              {k.hash.slice(0, 7)} <span className="faint">{fmtAgo(k.agoSecs)}</span>
            </span>
          </span>
        </button>
      ))}
    </aside>
  );
}
