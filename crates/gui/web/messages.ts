// Turning Claude Code's stream of events into what the conversation shows.
import type { Event } from './api';

export interface ToolResult {
  text: string;
  isError: boolean;
}

export interface Denial {
  tool: string;
  input: Record<string, unknown>;
}

export type Item =
  | { kind: 'user'; key: string; text: string }
  | { kind: 'text'; key: string; text: string }
  | { kind: 'tool'; key: string; id: string; name: string; input: Record<string, unknown>; result?: ToolResult }
  | { kind: 'denials'; key: string; denials: Denial[] }
  | { kind: 'error'; key: string; text: string };

function resultText(content: unknown): string {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map((c) => (c && typeof c === 'object' && 'text' in c ? String((c as { text: unknown }).text) : ''))
      .filter(Boolean)
      .join('\n');
  }
  return '';
}

export function toItems(events: Event[]): Item[] {
  const items: Item[] = [];
  const tools = new Map<string, Extract<Item, { kind: 'tool' }>>();
  events.forEach((e, i) => {
    switch (e.type) {
      case 'fleet_user':
        items.push({ kind: 'user', key: `u${i}`, text: String(e.text ?? '') });
        break;
      case 'fleet_error':
        items.push({ kind: 'error', key: `e${i}`, text: String(e.text ?? '') });
        break;
      case 'assistant': {
        const content = e.message?.content;
        if (!Array.isArray(content)) break;
        content.forEach((c, j) => {
          if (c?.type === 'text' && typeof c.text === 'string' && c.text.trim()) {
            items.push({ kind: 'text', key: `t${i}-${j}`, text: c.text });
          } else if (c?.type === 'tool_use') {
            const tool = { kind: 'tool' as const, key: `x${i}-${j}`, id: String(c.id), name: String(c.name), input: c.input ?? {} };
            tools.set(tool.id, tool);
            items.push(tool);
          }
        });
        break;
      }
      case 'user': {
        const content = e.message?.content;
        if (!Array.isArray(content)) break;
        for (const c of content) {
          if (c?.type !== 'tool_result') continue;
          const tool = tools.get(String(c.tool_use_id));
          if (tool) tool.result = { text: resultText(c.content), isError: !!c.is_error };
        }
        break;
      }
      case 'result': {
        const denials = Array.isArray(e.permission_denials) ? e.permission_denials : [];
        if (denials.length) {
          items.push({
            kind: 'denials',
            key: `d${i}`,
            denials: denials.map((d: Event) => ({ tool: String(d.tool_name), input: d.tool_input ?? {} })),
          });
        }
        if (e.is_error && typeof e.result === 'string') {
          items.push({ kind: 'error', key: `r${i}`, text: e.result });
        }
        break;
      }
    }
  });
  return items;
}

/// The one line that says what a tool call is about.
export function toolSubject(name: string, input: Record<string, unknown>, cwd: string): string {
  const s = (k: string) => (typeof input[k] === 'string' ? (input[k] as string) : '');
  const path = s('file_path') || s('notebook_path') || s('path');
  if (path) return relative(path, cwd);
  if (name === 'Bash' || name === 'PowerShell') return s('command');
  if (s('pattern')) return s('pattern');
  if (s('url')) return s('url');
  if (s('query')) return s('query');
  if (s('description')) return s('description');
  if (name === 'TodoWrite' && Array.isArray(input.todos)) return `${input.todos.length} items`;
  const first = Object.values(input).find((v) => typeof v === 'string');
  return typeof first === 'string' ? first : '';
}

export function relative(path: string, cwd: string): string {
  const norm = (p: string) => p.replace(/\\/g, '/');
  const p = norm(path);
  const c = norm(cwd).replace(/\/$/, '');
  return p.toLowerCase().startsWith(c.toLowerCase() + '/') ? p.slice(c.length + 1) : p;
}

/// The permission rule that lets a refused call through next time:
/// `Bash(npm:*)` for a command, the bare tool name otherwise.
export function allowRule(d: Denial): string {
  if (d.tool === 'Bash' && typeof d.input.command === 'string') {
    const word = d.input.command.trim().split(/\s+/)[0];
    if (word) return `Bash(${word}:*)`;
  }
  return d.tool;
}

export interface DiffLine {
  sign: ' ' | '+' | '-';
  text: string;
}

/// An Edit's change as removed and added lines.
export function editDiff(input: Record<string, unknown>): DiffLine[] | null {
  const pairs: Array<[string, string]> = [];
  if (typeof input.old_string === 'string' && typeof input.new_string === 'string') {
    pairs.push([input.old_string, input.new_string]);
  } else if (Array.isArray(input.edits)) {
    for (const e of input.edits) {
      if (e && typeof e.old_string === 'string' && typeof e.new_string === 'string') {
        pairs.push([e.old_string, e.new_string]);
      }
    }
  }
  if (!pairs.length) return null;
  const out: DiffLine[] = [];
  for (const [a, b] of pairs) {
    for (const t of a.split('\n')) out.push({ sign: '-', text: t });
    for (const t of b.split('\n')) out.push({ sign: '+', text: t });
  }
  return out;
}

export function fmtDuration(secs: number): string {
  const m = Math.floor(secs / 60);
  if (m < 1) return 'now';
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ${m % 60}m`;
  return h % 24 ? `${Math.floor(h / 24)}d ${h % 24}h` : `${Math.floor(h / 24)}d`;
}

export function fmtAgo(secs: number): string {
  const m = Math.floor(secs / 60);
  if (m < 1) return 'now';
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}
