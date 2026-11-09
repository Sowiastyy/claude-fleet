// The models and permission modes a session can run with.
import { Feather, FilePen, Gem, ListChecks, Rabbit, Shield, ShieldOff, Sparkles, Wand2, Zap } from 'lucide-react';
import type { Choice } from './components/Picker';

const I = 14;

export const MODELS: Choice[] = [
  { value: '', label: 'Default', icon: <Sparkles size={I} aria-hidden /> },
  { value: 'claude-fable-5-1', label: 'Fable 5.1', hint: 'claude-fable-5-1', icon: <Gem size={I} aria-hidden />, divide: true },
  { value: 'opus', label: 'Opus', hint: 'opus', icon: <Wand2 size={I} aria-hidden /> },
  { value: 'opus[1m]', label: 'Opus 1M', hint: 'opus[1m]', icon: <Wand2 size={I} aria-hidden /> },
  { value: 'sonnet', label: 'Sonnet', hint: 'sonnet', icon: <Feather size={I} aria-hidden /> },
  { value: 'sonnet[1m]', label: 'Sonnet 1M', hint: 'sonnet[1m]', icon: <Feather size={I} aria-hidden /> },
  { value: 'haiku', label: 'Haiku', hint: 'haiku', icon: <Rabbit size={I} aria-hidden /> },
];

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

/// The option a model a session reports belongs to: `claude-opus-5-5[1m]`
/// is Opus 1M.
export function modelChoice(model: string): string {
  if (MODELS.some((m) => m.value === model)) return model;
  const m = model.toLowerCase();
  const wide = m.includes('[1m]');
  if (m.includes('fable')) return 'claude-fable-5-1';
  for (const family of ['opus', 'sonnet']) if (m.includes(family)) return wide ? `${family}[1m]` : family;
  if (m.includes('haiku')) return 'haiku';
  return '';
}

/// `claude-opus-5-5[1m]` as `Opus 5.5 1M`; an alias as its option's label.
export function modelName(model: string): string {
  const full = /^claude-([a-z]+)-(\d+)(?:-(\d+))?(?:-\d{8})?(\[1m\])?$/i.exec(model);
  if (full) {
    const [, family, major, minor, wide] = full;
    const name = family[0].toUpperCase() + family.slice(1);
    return `${name} ${major}${minor ? `.${minor}` : ''}${wide ? ' 1M' : ''}`;
  }
  return MODELS.find((c) => c.value === model)?.label ?? model;
}
