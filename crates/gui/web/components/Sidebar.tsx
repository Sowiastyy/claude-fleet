import { ArrowDownToLine, ChevronsUpDown, LoaderCircle, Plus, RotateCw, Sailboat, Search, X } from 'lucide-react';
import type { Accounts, Foreign, Limits, Session, Status, UpdateState } from '../api';
import { fmtDuration } from '../messages';

const STATUS: Record<Status, string> = {
  working: 'working',
  idle: 'idle',
  waiting: 'needs you',
  starting: 'starting',
};

export function statusLabel(s: Session): string {
  if (s.status === 'waiting' && s.waitingFor) return `needs you: ${s.waitingFor}`;
  return STATUS[s.status];
}

interface Props {
  sessions: Session[];
  foreign: Foreign[];
  activeId: string | null;
  accounts: Accounts | null;
  accountsOpen: boolean;
  onHome: () => void;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onAccounts: () => void;
  update: UpdateState | null;
  onUpdate: () => void;
  onRestart: () => void;
  onPalette: () => void;
}

export function Sidebar(p: Props) {
  const me = p.accounts?.accounts.find((a) => a.current);
  const label = me?.email ?? (p.accounts?.current ? 'Signing in' : 'Not signed in');
  return (
    <aside className="sidebar">
      <div className="brand">
        <Sailboat size={22} color="var(--accent)" aria-hidden />
        <span className="brand-name">Fleet</span>
      </div>

      <button type="button" className="account-btn" onClick={p.onAccounts} aria-expanded={p.accountsOpen} aria-label="Switch account">
        <span className="avatar">{(me?.email ?? '?')[0].toUpperCase()}</span>
        <span className="stack">
          <span className="ellipsis" style={{ fontWeight: 500 }}>{label}</span>
          {me?.plan && <span className="small mist">{me.plan}</span>}
        </span>
        <ChevronsUpDown size={16} color="var(--mist)" aria-hidden />
      </button>

      <nav className="nav" aria-label="Fleet">
        <button type="button" className="nav-item" aria-current={p.activeId === null} onClick={p.onHome}>
          <Plus size={16} aria-hidden />
          <span style={{ flex: 1 }}>New session</span>
          {p.sessions.length < 9 && <kbd>F{p.sessions.length + 1}</kbd>}
        </button>
        <button type="button" className="nav-item" onClick={p.onPalette}>
          <Search size={16} aria-hidden />
          <span style={{ flex: 1 }}>Search</span>
          <kbd>Ctrl K</kbd>
        </button>
      </nav>

      <div className="scroll">
        <div className="section-head">
          <h2>Sessions</h2>
          <span className="small faint">{p.sessions.length}</span>
        </div>
        <ul className="list">
          {p.sessions.map((s, i) => (
            <li key={s.id} className="session-row">
              <button
                type="button"
                className="session-btn"
                aria-current={s.id === p.activeId}
                onClick={() => p.onSelect(s.id)}
              >
                <span className={`dot ${s.status}`} aria-hidden />
                <span className="stack" style={{ gap: 1 }}>
                  <span className="ellipsis" style={{ fontWeight: 500 }}>{s.name}</span>
                  <span className="small faint ellipsis">{s.cwd}</span>
                  <span className={`small status-text ${s.status}`}>
                    {statusLabel(s)}
                    {s.view === 'terminal' ? ', terminal' : ''}
                  </span>
                </span>
                {i < 9 && <kbd style={{ marginTop: 2 }}>F{i + 1}</kbd>}
              </button>
              <button type="button" className="row-close" aria-label={`Close ${s.name}`} onClick={() => p.onClose(s.id)}>
                <X size={14} aria-hidden />
              </button>
            </li>
          ))}
        </ul>

        {p.foreign.length > 0 && (
          <>
            <div className="section-head">
              <h2 style={{ color: 'var(--mist)' }}>Started elsewhere</h2>
              <span className="small faint">{p.foreign.length}</span>
            </div>
            {p.foreign.map((f, i) => (
              <div key={`${f.name}-${i}`} className="foreign-row" title={f.cwd}>
                <span className="dot foreign" aria-hidden />
                <span className="ellipsis" style={{ flex: 1 }}>{f.name}</span>
                <span className="small">{f.status === 'busy' ? 'working' : f.status === 'waiting' ? 'needs you' : f.status}</span>
              </div>
            ))}
          </>
        )}
      </div>

      {p.update && (p.update.available || p.update.installed) && (
        <UpdateBox u={p.update} running={p.sessions.length} onUpdate={p.onUpdate} onRestart={p.onRestart} />
      )}
      {p.accounts?.live && <LimitsBox limits={p.accounts.live} />}
    </aside>
  );
}

function LimitsBox({ limits }: { limits: Limits }) {
  const rows: Array<[string, Limits['session']]> = [
    ['5-hour limit', limits.session],
    ['Weekly limit', limits.weekly],
  ];
  return (
    <div className="limits">
      {rows.map(([name, w]) =>
        w ? (
          <div className="limit" key={name}>
            <div className="limit-head">
              <span style={{ color: 'var(--fog-2)' }}>{name}</span>
              <span className="mist">
                {w.expired ? 'reset' : `${w.pct}%`}
                {!w.expired && w.resetsInSecs != null ? `, ${fmtDuration(w.resetsInSecs)}` : ''}
              </span>
            </div>
            <div className="bar">
              <span style={{ width: `${w.expired ? 0 : Math.min(100, w.pct)}%` }} />
            </div>
          </div>
        ) : null,
      )}
    </div>
  );
}

function UpdateBox({ u, running, onUpdate, onRestart }: { u: UpdateState; running: number; onUpdate: () => void; onRestart: () => void }) {
  if (u.installed) {
    return (
      <button type="button" className="update-btn ready" onClick={onRestart}>
        <RotateCw size={15} aria-hidden />
        <span style={{ flex: 1 }}>Restart to {u.available}</span>
        {running > 0 && <span className="small">{running} reopen</span>}
      </button>
    );
  }
  return (
    <button type="button" className="update-btn" disabled={u.installing} onClick={onUpdate}>
      {u.installing ? <LoaderCircle size={15} className="spin" aria-hidden /> : <ArrowDownToLine size={15} aria-hidden />}
      <span style={{ flex: 1 }}>{u.installing ? `Updating to ${u.available}` : `Update to ${u.available}`}</span>
      <span className="small faint">v{u.current}</span>
    </button>
  );
}
