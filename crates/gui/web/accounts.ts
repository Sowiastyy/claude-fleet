// Which account a session runs as, and the colour that tells them apart.
import type { Account, Accounts } from './api';

/// Distinct enough side by side, readable on every theme.
const HUES = ['#e2b867', '#6fa8ff', '#5fcf9a', '#f07a8c', '#b58cff', '#3cc6d6', '#f29a4a'];

export function accountOf(accounts: Accounts | null, uuid: string): Account | undefined {
  return accounts?.accounts.find((a) => a.uuid === uuid);
}

/// The account's colour, by its place in the (email-sorted) list, so it
/// stays the same between runs.
export function accountColor(accounts: Accounts | null, uuid: string): string {
  const i = accounts?.accounts.findIndex((a) => a.uuid === uuid) ?? -1;
  return i < 0 ? 'var(--faint)' : HUES[i % HUES.length];
}

export function accountInitial(a: Account | undefined): string {
  return (a?.email || a?.name || '?')[0].toUpperCase();
}

/// What to call it in a tight spot: the part of the email before the @.
export function accountShort(a: Account | undefined): string {
  if (!a) return 'Signed-in account';
  return a.email.split('@')[0] || a.email;
}
