import type { CSSProperties } from 'react';
import type { Accounts } from '../api';
import { accountColor, accountInitial } from '../accounts';
import type { Choice } from './Picker';

/// The saved accounts as a picker's options: coloured initial, email, and
/// how much of the 5-hour limit is spent.
export function accountChoices(accounts: Accounts | null): Choice[] {
  return (accounts?.accounts ?? []).map((a) => {
    const w = a.limits?.session;
    const used = w && !w.expired ? `${w.pct}% used` : '';
    return {
      value: a.uuid,
      label: a.email,
      hint: a.needsLogin ? 'login expired' : [a.current ? 'default' : a.plan, used].filter(Boolean).join(', '),
      tone: a.needsLogin ? 'danger' : undefined,
      icon: (
        <span className="acct-chip sm" style={{ '--acct': accountColor(accounts, a.uuid) } as CSSProperties}>
          {accountInitial(a)}
        </span>
      ),
    };
  });
}
