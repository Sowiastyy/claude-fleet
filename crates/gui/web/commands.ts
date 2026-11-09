// Claude Code's slash commands in the message box: what `/` offers, and
// where a typed command goes.
import { api, type SlashCommand } from './api';

// Each folder's list, kept for a while: asking for it starts a process.
const cache = new Map<string, { at: number; list: Promise<SlashCommand[]> }>();

export function commandsFor(cwd: string): Promise<SlashCommand[]> {
  const hit = cache.get(cwd);
  if (hit && Date.now() - hit.at < 5 * 60_000) return hit.list;
  const list = api.commands(cwd).catch(() => [] as SlashCommand[]);
  cache.set(cwd, { at: Date.now(), list });
  return list;
}

/// The command word being typed: the text so far is `/` and a word, and
/// the caret has not left it.
export function slashAt(text: string, caret: number): string | null {
  const m = /^\/(\S*)$/.exec(text.slice(0, caret));
  return m ? m[1] : null;
}

/// Commands for what follows `/`: the name first, then an alias, then the
/// letters anywhere in the name, then the description.
export function rankCommands(list: SlashCommand[], query: string): SlashCommand[] {
  const q = query.toLowerCase();
  const scored: Array<[number, SlashCommand]> = [];
  for (const c of list) {
    const name = c.name.toLowerCase();
    // A plugin's command answers to its short name too: `caveman:caveman-help`.
    const short = name.slice(name.lastIndexOf(':') + 1);
    let score = -1;
    if (!q) score = c.terminal ? 1 : 0;
    else if (name.startsWith(q) || short.startsWith(q)) score = 0;
    else if (c.aliases.some((a) => a.toLowerCase().startsWith(q))) score = 1;
    else if (q.length >= 2 && name.includes(q)) score = 2;
    // A word of the description, once there is enough typed to mean one.
    else if (q.length >= 3 && c.description.toLowerCase().split(/[^a-z0-9]+/).some((w) => w.startsWith(q))) score = 3;
    if (score >= 0) scored.push([score, c]);
  }
  scored.sort((a, b) => a[0] - b[0] || a[1].name.length - b[1].name.length || a[1].name.localeCompare(b[1].name));
  return scored.map(([, c]) => c);
}

export function parseSlash(text: string): { name: string; args: string } | null {
  const m = /^\/([\w:.-]+)(?:\s+([\s\S]*))?$/.exec(text.trim());
  return m ? { name: m[1].toLowerCase(), args: (m[2] ?? '').trim() } : null;
}

/// Where a message that is a slash command goes: some the app does itself
/// (they change what it keeps about the session), the terminal-only ones to
/// the session's terminal, everything else to Claude Code in the chat.
export type Route = { kind: 'chat' } | { kind: 'terminal' } | { kind: 'clear' } | { kind: 'model'; model: string };

export function routeSlash(text: string, list: SlashCommand[]): Route {
  const c = parseSlash(text);
  if (!c) return { kind: 'chat' };
  if (c.name === 'clear' || c.name === 'reset' || c.name === 'new') return { kind: 'clear' };
  // With no model named, Claude Code shows its picker, which only the
  // terminal can.
  if (c.name === 'model') return c.args ? { kind: 'model', model: c.args.split(/\s+/)[0] } : { kind: 'terminal' };
  const known = list.find((x) => x.name.toLowerCase() === c.name || x.aliases.some((a) => a.toLowerCase() === c.name));
  return known?.terminal ? { kind: 'terminal' } : { kind: 'chat' };
}

/// A cloud session's id or claude.ai/code link, as opposed to a task.
export function isCloudRef(text: string): boolean {
  const t = text.trim();
  return /^(https?:\/\/)?claude\.ai\/code\/(cse|session)_[\w-]{4,}\S*$/.test(t) || /^(cse|session)_[\w-]{4,}$/.test(t);
}
