import { Plus, Trash2 } from 'lucide-react';
import type { Account, Accounts, LimitWindow } from '../api';

interface Props {
  accounts: Accounts;
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
  return (
    <>
      <button type="button" className="scrim" aria-label="Close accounts" onClick={p.onClose} />
      <div className="menu" role="dialog" aria-label="Accounts">
        <div style={{ height: 6 }} />
        {p.accounts.accounts.map((a) => (
          <div key={a.uuid} className="acct">
            <button type="button" className="acct-btn" aria-current={a.current} onClick={() => p.onChoose(a)}>
              <span className="avatar" style={{ color: a.current ? 'var(--accent)' : 'var(--fog-2)' }}>
                {a.email[0]?.toUpperCase()}
              </span>
              <span className="stack">
                <span style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <span className="ellipsis" style={{ fontWeight: 500 }}>{a.email}</span>
                  <span className="small" style={{ marginLeft: 'auto', color: a.current ? 'var(--idle)' : 'var(--mist)' }}>
                    {a.current ? 'Signed in' : a.plan}
                  </span>
                </span>
                {a.org && <span className="small faint ellipsis">{a.org}</span>}
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
            {!a.current && (
              <button type="button" className="acct-forget" aria-label={`Forget ${a.email}`} onClick={() => p.onForget(a)}>
                <Trash2 size={14} aria-hidden />
              </button>
            )}
          </div>
        ))}
        <div className="menu-foot">
          <span className="small mist">Running sessions</span>
          <div className="segmented" role="group" aria-label="Running sessions">
            <button type="button" aria-pressed={p.carry} onClick={() => p.onCarry(true)}>Move over</button>
            <button type="button" aria-pressed={!p.carry} onClick={() => p.onCarry(false)}>Stay as they are</button>
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
