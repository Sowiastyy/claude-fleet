import type { CSSProperties } from 'react';
import { ArrowDownToLine, ChevronsUpDown, Cloud, LoaderCircle, Plus, RotateCw, Sailboat, Search, SquareTerminal, X } from 'lucide-react';
import type { Accounts, Limits, LimitWindow, Session, Status, UpdateState } from '../api';
import { accountColor, accountInitial, accountOf } from '../accounts';
import { fmtDuration } from '../messages';

const STATUS: Record<Status, string> = {
  working: 'working',
  idle: 'idle',
  waiting: 'needs you',
  starting: 'starting',
};

export function statusLabel(s: Session): string {
  if (s.shell) return s.alive ? 'shell' : 'exited';
  // Attached without a process of its own: the cloud has it.
  if (s.cloud && s.cloudId && !s.alive) return 'on claude.ai';
  if (s.status === 'waiting' && s.waitingFor) return `needs you: ${s.waitingFor}`;
  return STATUS[s.status];
}

/// What a session is called in lists: a cloud session by its task, which
/// says more than the folder it was started from.
export function sessionTitle(s: Session): string {
  return s.cloud && s.cloudTask ? s.cloudTask : s.name;
}

/// What follows the status in a session's row.
function kindNote(s: Session): string {
  if (s.shell) return '';
  if (s.cloud) return s.alive || !s.cloudId ? ', cloud' : '';
  return s.view === 'terminal' ? ', terminal' : '';
}

interface Props {
  sessions: Session[];
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
  onShell: () => void;
}

interface Group {
  account: string;
  rows: Array<{ s: Session; i: number }>;
}

/// The sessions by the account they run as: the signed-in one first, then
/// the others in the dropdown's order, `~/.claude`'s own (a login) last.
function byAccount(sessions: Session[], accounts: Accounts | null): Group[] {
  const order = (uuid: string) => {
    if (!uuid) return 1e6;
    if (uuid === accounts?.current) return -1;
    const i = accounts?.accounts.findIndex((a) => a.uuid === uuid) ?? -1;
    return i < 0 ? 1e5 : i;
  };
  const groups = new Map<string, Group>();
  sessions.forEach((s, i) => {
    const g = groups.get(s.account) ?? { account: s.account, rows: [] };
    g.rows.push({ s, i });
    groups.set(s.account, g);
  });
  return [...groups.values()].sort((a, b) => order(a.account) - order(b.account));
}

export function Sidebar(p: Props) {
  const me = p.accounts?.accounts.find((a) => a.current);
  const label = me?.email ?? (p.accounts?.current ? 'Signing in' : 'Not signed in');
  const several = (p.accounts?.accounts.length ?? 0) > 1;
  const groups = byAccount(p.sessions, p.accounts);
  const grouped = groups.length > 1 || (several && groups.length === 1 && groups[0].account !== p.accounts?.current);
  const row = ({ s, i }: { s: Session; i: number }) => (
    <li
      key={s.id}
      className="session-row"
      style={several ? ({ '--acct': accountColor(p.accounts, s.account) } as CSSProperties) : undefined}
    >
      <button
        type="button"
        className={`session-btn ${several ? 'tinted' : ''}`}
        aria-current={s.id === p.activeId}
        onClick={() => p.onSelect(s.id)}
      >
        <span className="stack" style={{ gap: 1 }}>
          <span className={`session-name ${s.status === 'working' ? 'shimmer' : ''}`} title={sessionTitle(s)}>
            {s.cloud && <Cloud size={13} aria-label="cloud" className="cloud-mark" />}
            {s.shell && <SquareTerminal size={13} aria-label="terminal" className="shell-mark" />}
            <span className="ellipsis" style={{ fontWeight: 500 }}>{s.name}</span>
            <span className={`small status-text ${s.status} ${s.shell && !s.alive ? 'exited' : ''}`}>
              {statusLabel(s)}
              {kindNote(s)}
            </span>
          </span>
          {s.cloud && s.cloudTask && <span className="small faint ellipsis">{s.cloudTask}</span>}
        </span>
        {i < 9 && <kbd>F{i + 1}</kbd>}
      </button>
      <button type="button" className="row-close" aria-label={`Close ${sessionTitle(s)}`} onClick={() => p.onClose(s.id)}>
        <X size={14} aria-hidden />
      </button>
    </li>
  );
  return (
    <aside className="sidebar">
      <div className="brand">
        <Sailboat size={22} color="var(--accent)" aria-hidden />
        <span className="brand-name">Fleet</span>
        {p.update?.current && (
          <span className="brand-version" title={`Claude Fleet ${p.update.current}`}>
            v{p.update.current}
          </span>
        )}
      </div>

      <button type="button" className="account-btn" onClick={p.onAccounts} aria-expanded={p.accountsOpen} aria-label="Switch account">
        <span className="avatar" style={several && me ? { color: accountColor(p.accounts, me.uuid) } : undefined}>
          {accountInitial(me)}
        </span>
        <span className="stack">
          <span className="ellipsis" style={{ fontWeight: 500 }}>{label}</span>
          <span className="small mist ellipsis">
            {several ? `New sessions${me?.plan ? `, ${me.plan}` : ''}` : me?.plan}
          </span>
        </span>
        <ChevronsUpDown size={16} color="var(--mist)" aria-hidden />
      </button>

      <nav className="nav" aria-label="Fleet">
        <button type="button" className="nav-item" aria-current={p.activeId === null} onClick={p.onHome}>
          <Plus size={16} aria-hidden />
          <span style={{ flex: 1 }}>New session</span>
          {p.sessions.length < 9 && <kbd>F{p.sessions.length + 1}</kbd>}
        </button>
        <button type="button" className="nav-item" onClick={p.onShell} title="A shell in the current session's folder">
          <SquareTerminal size={16} aria-hidden />
          <span style={{ flex: 1 }}>New terminal</span>
          <kbd>Ctrl Shift `</kbd>
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
        {grouped ? (
          groups.map((g) => (
            <section key={g.account || 'login'} className="acct-group" aria-label={accountOf(p.accounts, g.account)?.email ?? 'Signed-in account'}>
              <AccountHead accounts={p.accounts} uuid={g.account} count={g.rows.length} />
              <ul className="list">{g.rows.map(row)}</ul>
            </section>
          ))
        ) : (
          <ul className="list">{p.sessions.map((s, i) => row({ s, i }))}</ul>
        )}
      </div>

      {p.update && (p.update.available || p.update.installed) && (
        <UpdateBox u={p.update} running={p.sessions.length} onUpdate={p.onUpdate} onRestart={p.onRestart} />
      )}
      {(me?.limits ?? p.accounts?.live) && <LimitsBox limits={(me?.limits ?? p.accounts?.live)!} />}
    </aside>
  );
}

/// A group's header: whose sessions these are, and how much of its limits
/// is left, at a glance.
function AccountHead({ accounts, uuid, count }: { accounts: Accounts | null; uuid: string; count: number }) {
  const a = accountOf(accounts, uuid);
  const color = accountColor(accounts, uuid);
  const pct = (w: LimitWindow | null | undefined) => (w && !w.expired ? w.pct : w ? 0 : null);
  const five = pct(a?.limits?.session);
  const week = pct(a?.limits?.weekly);
  return (
    <div className="acct-head" style={{ '--acct': color } as CSSProperties} title={a?.email}>
      <span className="acct-chip">{accountInitial(a)}</span>
      <span className="ellipsis acct-head-name">{a ? a.email : uuid ? 'Forgotten account' : 'Signed-in account'}</span>
      {a?.current && <span className="acct-default">default</span>}
      {five != null && (
        <span className={`acct-meter ${five >= 85 ? 'hot' : five >= 50 ? 'warm' : ''}`} title={`5-hour limit ${five}%${week != null ? `, weekly ${week}%` : ''}`}>
          <span style={{ width: `${Math.min(100, five)}%` }} />
        </span>
      )}
      <span className="small faint">{count}</span>
    </div>
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
