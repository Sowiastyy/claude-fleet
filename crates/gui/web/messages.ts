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

export type ToolItem = { kind: 'tool'; key: string; id: string; name: string; input: Record<string, unknown>; result?: ToolResult };
export type ThinkingItem = { kind: 'thinking'; key: string; text: string };

export type Item =
  | { kind: 'user'; key: string; text: string; images: string[]; ts?: number }
  | { kind: 'note'; key: string; text: string }
  | { kind: 'text'; key: string; text: string }
  | ToolItem
  | ThinkingItem
  | { kind: 'denials'; key: string; denials: Denial[] }
  | { kind: 'meta'; key: string; durationMs: number | null; costUsd: number | null }
  | { kind: 'error'; key: string; text: string };

/// What the thread draws: items, with each run of tool calls and thinking
/// folded into one block of steps.
export type Block = Item | { kind: 'steps'; key: string; items: Array<ToolItem | ThinkingItem> };

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
        items.push({
          kind: 'user',
          key: `u${i}`,
          text: String(e.text ?? ''),
          images: Array.isArray(e.images) ? e.images.filter((u: unknown) => typeof u === 'string') : [],
          ts: typeof e.ts === 'number' ? e.ts : undefined,
        });
        break;
      case 'fleet_note':
        items.push({ kind: 'note', key: `n${i}`, text: String(e.text ?? '') });
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
          } else if (c?.type === 'thinking' && typeof c.thinking === 'string' && c.thinking.trim()) {
            items.push({ kind: 'thinking', key: `k${i}-${j}`, text: c.thinking });
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
        items.push({
          kind: 'meta',
          key: `m${i}`,
          durationMs: typeof e.duration_ms === 'number' ? e.duration_ms : null,
          costUsd: typeof e.total_cost_usd === 'number' ? e.total_cost_usd : null,
        });
        break;
      }
    }
  });
  return items;
}

/// Fold each run of two or more tool calls and thoughts into one block.
export function toBlocks(items: Item[]): Block[] {
  const out: Block[] = [];
  let run: Array<ToolItem | ThinkingItem> = [];
  const flush = () => {
    if (run.length > 1) out.push({ kind: 'steps', key: `s-${run[0].key}`, items: run });
    else out.push(...run);
    run = [];
  };
  for (const it of items) {
    if (it.kind === 'tool' || it.kind === 'thinking') run.push(it);
    else {
      flush();
      out.push(it);
    }
  }
  flush();
  return out;
}

const VERBS: Array<[RegExp, string, string]> = [
  [/^(Edit|MultiEdit|Write|NotebookEdit)$/, 'edited', 'file'],
  [/^Read$/, 'read', 'file'],
  [/^(Bash|PowerShell)$/, 'ran', 'command'],
  [/^(Grep|Glob|WebSearch)$/, 'searched', 'time'],
  [/^WebFetch$/, 'fetched', 'page'],
  [/^(Task|Agent)$/, 'started', 'agent'],
  [/^TodoWrite$/, 'updated', 'plan'],
];

/// `Thought, read 3 files, ran 2 commands`.
export function stepsSummary(items: Array<ToolItem | ThinkingItem>): string {
  // verb+noun -> what was counted; edits count files, everything else calls.
  const seen = new Map<string, { verb: string; noun: string; keys: Set<string> }>();
  let thought = false;
  let other = 0;
  for (const it of items) {
    if (it.kind === 'thinking') {
      thought = true;
      continue;
    }
    const v = VERBS.find(([re]) => re.test(it.name));
    if (!v) {
      other += 1;
      continue;
    }
    const [, verb, noun] = v;
    const entry = seen.get(verb + noun) ?? { verb, noun, keys: new Set<string>() };
    entry.keys.add(verb === 'edited' ? editPath(it.input) : it.key);
    seen.set(verb + noun, entry);
  }
  const parts = [...seen.values()].map(({ verb, noun, keys }) =>
    noun === 'plan' ? `${verb} the plan` : `${verb} ${keys.size} ${noun}${keys.size === 1 ? '' : 's'}`,
  );
  if (other) parts.push(`${other} other ${other === 1 ? 'tool' : 'tools'}`);
  if (thought) parts.unshift('thought');
  const s = parts.join(', ');
  return s.charAt(0).toUpperCase() + s.slice(1);
}

export function editPath(input: Record<string, unknown>): string {
  const p = input.file_path ?? input.notebook_path ?? input.path;
  return typeof p === 'string' ? p : '';
}

/// Edited files in a run of steps, with lines added and removed.
export function editedFiles(items: Array<ToolItem | ThinkingItem>): Array<{ path: string; added: number; removed: number }> {
  const files = new Map<string, { path: string; added: number; removed: number }>();
  for (const it of items) {
    if (it.kind !== 'tool' || !VERBS[0][0].test(it.name)) continue;
    const path = editPath(it.input);
    const f = files.get(path) ?? { path, added: 0, removed: 0 };
    const diff = editDiff(it.input);
    if (diff) {
      f.added += diff.filter((l) => l.sign === '+').length;
      f.removed += diff.filter((l) => l.sign === '-').length;
    } else if (typeof it.input.content === 'string') {
      f.added += it.input.content.split('\n').length;
    }
    files.set(path, f);
  }
  return [...files.values()];
}

export interface Todo {
  content: string;
  status: 'pending' | 'in_progress' | 'completed';
  activeForm?: string;
}

/// The plan as the last TodoWrite left it.
export function latestTodos(items: Item[]): Todo[] | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    if (it.kind === 'tool' && it.name === 'TodoWrite' && Array.isArray(it.input.todos)) {
      return (it.input.todos as Todo[]).filter((t) => t && typeof t.content === 'string');
    }
  }
  return null;
}

/// The call still running at the end of the thread, if any.
export function runningTool(items: Item[]): ToolItem | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    if (it.kind === 'user') return null;
    if (it.kind === 'tool' && !it.result) return it;
  }
  return null;
}

/// When the turn in progress started: the last message's time.
export function turnStart(items: Item[]): number | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    if (it.kind === 'user') return it.ts ?? null;
  }
  return null;
}

export function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m}m ${s % 60}s` : `${Math.floor(m / 60)}h ${m % 60}m`;
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
