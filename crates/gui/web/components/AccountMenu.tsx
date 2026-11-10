import { Plus, Trash2 } from 'lucide-react';
import type { CSSProperties } from 'react';
import type { Account, Accounts, LimitWindow, Session } from '../api';
import { accountColor, accountInitial } from '../accounts';

interface Props {
  accounts: Accounts;
  sessions: Session[];
  carry: boolean;
  onCarry: (carry: boolean) => void;
  onChoose: (a: Account) => void;
  onForget: (a: Account) => void;
  onAdd: () => void;
  onClose: () => void;
}

function pctColor(w: LimitWindow): string {
  if (w.expired) return 'var(--idle)';
  return w.pct >= 85 ? 'var(--danger)' : w.pct >= 50 ? 'var(--warn)' : 'var(--idle)';
}

function Window({ name, w }: { name: string; w: LimitWindow | null }) {
  if (!w) return <span />;
  const pct = w.expired ? 0 : Math.min(100, w.pct);
  return (
    <span style={{ display: 'flex', flexDirection: 'column', gap: 3 }}>
      <span style={{ display: 'flex', justifyContent: 'space-between' }}>
        <span>{name}</span>
        <span style={{ color: pctColor(w) }}>{w.expired ? 'reset' : `${w.pct}%`}</span>
      </span>
      <span className="bar thin">
        <span style={{ width: `${pct}%` }} />
      </span>
    </span>
  );
}

export function AccountMenu(p: Props) {
  const several = p.accounts.accounts.length > 1;
  const running = (uuid: string) => p.sessions.filter((s) => s.account === uuid);
  return (
    <>
      <button type="button" className="scrim" aria-label="Close accounts" onClick={p.onClose} />
      <div className="menu" role="dialog" aria-label="Accounts">
        <div style={{ height: 6 }} />
        {several && (
          <p className="menu-note small mist">
            Each session keeps the account it started with, so several can run side by side. Picking one here makes it the
            default for new sessions.
          </p>
        )}
        {p.accounts.accounts.map((a) => {
          const on = running(a.uuid);
          const busy = on.filter((s) => s.status === 'working').length;
          return (
          <div key={a.uuid} className="acct">
            <button type="button" className="acct-btn" aria-current={a.current} onClick={() => p.onChoose(a)}>
              <span
                className="avatar"
                style={
                  several
                    ? ({ color: 'var(--ink)', background: accountColor(p.accounts, a.uuid) } as CSSProperties)
                    : { color: a.current ? 'var(--accent)' : 'var(--fog-2)' }
                }
              >
                {accountInitial(a)}
              </span>
              <span className="stack">
                <span style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <span className="ellipsis" style={{ fontWeight: 500 }}>{a.email}</span>
                  <span className="small" style={{ marginLeft: 'auto', color: a.current ? 'var(--idle)' : 'var(--mist)' }}>
                    {a.current ? (several ? 'Default' : 'Signed in') : a.plan}
                  </span>
                </span>
                {a.org && <span className="small faint ellipsis">{a.org}</span>}
                {on.length > 0 && (
                  <span className="small mist">
                    {on.length} session{on.length === 1 ? '' : 's'}
                    {busy > 0 ? `, ${busy} working` : ''}
                  </span>
                )}
                {a.needsLogin ? (
                  <span className="small" style={{ color: 'var(--danger)' }}>Login expired</span>
                ) : a.limits ? (
                  <span className="acct-limits">
                    <Window name="5-hour" w={a.limits.session} />
                    <Window name="Weekly" w={a.limits.weekly} />
                  </span>
                ) : null}
              </span>
            </button>
            {!a.current && on.length === 0 && (
              <button type="button" className="acct-forget" aria-label={`Forget ${a.email}`} onClick={() => p.onForget(a)}>
                <Trash2 size={14} aria-hidden />
              </button>
            )}
          </div>
          );
        })}
        <div className="menu-foot">
          <span className="small mist" title="Sessions in the middle of a turn, or waiting on you, always stay">
            On switch, sessions
          </span>
          <div className="segmented" role="group" aria-label="On switch, running sessions">
            <button type="button" aria-pressed={!p.carry} onClick={() => p.onCarry(false)}>Keep theirs</button>
            <button type="button" aria-pressed={p.carry} onClick={() => p.onCarry(true)}>Move idle over</button>
          </div>
        </div>
        <button type="button" className="menu-add" onClick={p.onAdd}>
          <Plus size={16} color="var(--accent)" aria-hidden />
          Add an account
        </button>
      </div>
    </>
  );
}
