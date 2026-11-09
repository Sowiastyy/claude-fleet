import { useState } from 'react';
import { ArrowDownToLine, ArrowUpFromLine, GitBranch, GitCommitHorizontal, Sparkles } from 'lucide-react';
import { api, errorText, type GitState } from '../api';
import { fmtAgo } from '../messages';

const CODE_COLOR: Record<string, string> = {
  M: 'var(--warn)',
  A: 'var(--idle)',
  D: 'var(--danger)',
  R: 'var(--waiting)',
  U: 'var(--idle)',
};

function mark(code: string): string {
  if (code === '??') return 'U';
  const c = code.trim()[0] ?? '?';
  return c;
}

interface Props {
  cwd: string;
  git: GitState | null;
  onRefresh: () => void;
  onToast: (text: string, bad?: boolean) => void;
}

export function GitPanel({ cwd, git, onRefresh, onToast }: Props) {
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

  if (!git) {
    return (
      <aside className="git" aria-label="Git">
        <div className="git-head">
          <span className="mist">Not a git repository</span>
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
          <span className="ellipsis">{git.branch ?? 'detached HEAD'}</span>
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
        return (
          <div className="change" key={c.path}>
            <span className="code" style={{ color: CODE_COLOR[m] ?? 'var(--mist)' }}>{m}</span>
            <span className="path" title={c.path}>{c.path}</span>
            {c.added != null && <span style={{ color: 'var(--idle)' }}>+{c.added}</span>}
            {c.removed != null && c.removed > 0 && <span style={{ color: 'var(--danger)' }}>−{c.removed}</span>}
          </div>
        );
      })}
      <div className="git-sub">
        <h3>History</h3>
      </div>
      {git.log.map((k, i) => (
        <div className="commit" key={k.hash}>
          <span className={`dot ${i < ahead ? 'ahead' : ''}`} aria-hidden />
          <span className="stack">
            <span className="ellipsis">{k.subject}</span>
            <span className="small" style={{ color: i < ahead ? 'var(--warn)' : 'var(--mist)' }}>
              {k.hash.slice(0, 7)} <span className="faint">{fmtAgo(k.agoSecs)}</span>
            </span>
          </span>
        </div>
      ))}
    </aside>
  );
}
