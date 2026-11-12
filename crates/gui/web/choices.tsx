// The models and permission modes a session can run with.
import { Feather, FilePen, Gem, ListChecks, Rabbit, Shield, ShieldOff, Sparkles, Wand2, Zap } from 'lucide-react';
import type { ReactNode } from 'react';
import type { Choice } from './components/Picker';

const I = 14;

/// The current models by their full ids, so the name on each option is the
/// model that runs, not whatever an alias points at today.
const MODEL_IDS: Array<[string, ReactNode]> = [
  ['claude-fable-5-2', <Gem size={I} aria-hidden />],
  ['claude-opus-5-5', <Wand2 size={I} aria-hidden />],
  ['claude-opus-5-5[1m]', <Wand2 size={I} aria-hidden />],
  ['claude-sonnet-5-5', <Feather size={I} aria-hidden />],
  ['claude-sonnet-5-5[1m]', <Feather size={I} aria-hidden />],
  ['claude-haiku-5-5', <Rabbit size={I} aria-hidden />],
];

export const MODELS: Choice[] = [
  { value: '', label: 'Default', icon: <Sparkles size={I} aria-hidden /> },
  ...MODEL_IDS.map(([id, icon], i) => ({
    value: id,
    label: modelName(id),
    hint: id.includes('fable') ? 'usage credits' : undefined,
    icon,
    divide: i === 0,
  })),
];

/// What the options used to be, and what a session started with an alias
/// reports before its first turn.
const ALIASES: Record<string, string> = {
  fable: 'claude-fable-5-2',
  opus: 'claude-opus-5-5',
  'opus[1m]': 'claude-opus-5-5[1m]',
  sonnet: 'claude-sonnet-5-5',
  'sonnet[1m]': 'claude-sonnet-5-5[1m]',
  haiku: 'claude-haiku-5-5',
};

export const MODES: Choice[] = [
  { value: 'default', label: 'Ask first', icon: <Shield size={I} aria-hidden /> },
  { value: 'acceptEdits', label: 'Accept edits', icon: <FilePen size={I} aria-hidden />, tone: 'accent' },
  { value: 'plan', label: 'Plan only', icon: <ListChecks size={I} aria-hidden />, tone: 'info' },
  { value: 'auto', label: 'Auto', icon: <Zap size={I} aria-hidden />, tone: 'accent' },
  { value: 'bypassPermissions', label: 'Ask nothing', icon: <ShieldOff size={I} aria-hidden />, tone: 'danger', divide: true },
];

/// What Shift+Tab steps through, as in Claude Code.
const CYCLE = ['default', 'acceptEdits', 'plan'];

export function nextMode(mode: string): string {
  const i = CYCLE.indexOf(mode || 'default');
  return CYCLE[(i + 1) % CYCLE.length];
}

/// The option a model belongs to: an alias or an id with a date stamp maps
/// to the id it stands for. A model with no option (an older version) is
/// returned as it is, so nothing in the list is ticked for it.
export function modelChoice(model: string): string {
  const m = model.trim().toLowerCase();
  if (!m) return '';
  if (ALIASES[m]) return ALIASES[m];
  const undated = m.replace(/-\d{8}(?=(\[1m\])?$)/, '');
  return MODELS.some((c) => c.value === undated) ? undated : model;
}

/// `claude-opus-5-5[1m]` as `Opus 5.5 1M`; an alias as the model it picks.
export function modelName(model: string): string {
  const full = /^claude-([a-z]+)-(\d+)(?:-(\d+))?(?:-\d{8})?(\[1m\])?$/i.exec(model);
  if (full) {
    const [, family, major, minor, wide] = full;
    const name = family[0].toUpperCase() + family.slice(1);
    return `${name} ${major}${minor ? `.${minor}` : ''}${wide ? ' 1M' : ''}`;
  }
  const alias = ALIASES[model.toLowerCase()];
  return alias ? modelName(alias) : model || 'Default';
}
