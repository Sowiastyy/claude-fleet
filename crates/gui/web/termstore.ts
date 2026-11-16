// Terminal output kept outside React: it arrives many times a second and a
// view that opens later has to replay what it missed.
type Listener = (chunk: Uint8Array) => void;

const LIMIT = 2 * 1024 * 1024;

/// The DEC private modes a replay has to bring back once the output that set
/// them has been dropped: the alternate screen first, then cursor keys and
/// visibility, mouse tracking and its encoding (Claude Code's history
/// scrolls by the wheel reaching it), focus events, bracketed paste.
const MODES = [1049, 1047, 47, 1, 25, 1000, 1002, 1003, 1004, 1005, 1006, 1015, 2004];
const MODE_SET = /\x1b\[\?([\d;]+)([hl])/g;
const latin1 = new TextDecoder('latin1');

/// What the dropped output left set.
interface Dropped {
  modes: Map<number, boolean>;
  /// Its last bytes, for a sequence cut between two chunks.
  tail: string;
}

class TermBuffer {
  private data = new Map<string, Uint8Array[]>();
  private sizes = new Map<string, number>();
  private dropped = new Map<string, Dropped>();
  private listeners = new Map<string, Set<Listener>>();

  push(id: string, b64: string) {
    const bin = atob(b64);
    const chunk = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) chunk[i] = bin.charCodeAt(i);
    const list = this.data.get(id) ?? [];
    list.push(chunk);
    let size = (this.sizes.get(id) ?? 0) + chunk.length;
    while (size > LIMIT && list.length > 1) {
      const old = list.shift()!;
      size -= old.length;
      this.drop(id, old);
    }
    this.data.set(id, list);
    this.sizes.set(id, size);
    this.listeners.get(id)?.forEach((l) => l(chunk));
  }

  private drop(id: string, chunk: Uint8Array) {
    const d = this.dropped.get(id) ?? { modes: new Map(), tail: '' };
    // A sequence wholly in the tail is seen twice, in order: no harm.
    const text = d.tail + latin1.decode(chunk);
    for (const m of text.matchAll(MODE_SET)) {
      for (const n of m[1].split(';')) {
        const mode = Number(n);
        if (MODES.includes(mode)) d.modes.set(mode, m[2] === 'h');
      }
    }
    d.tail = text.slice(-24);
    this.dropped.set(id, d);
  }

  /// What a new view replays: the modes the dropped part left set, then
  /// what is kept.
  chunks(id: string): Uint8Array[] {
    const list = this.data.get(id) ?? [];
    const modes = this.dropped.get(id)?.modes;
    if (!modes?.size) return list;
    const prefix = MODES.filter((m) => modes.has(m))
      .map((m) => `\x1b[?${m}${modes.get(m) ? 'h' : 'l'}`)
      .join('');
    return [new TextEncoder().encode(prefix), ...list];
  }

  /// A new process starts on a clean screen.
  reset(id: string) {
    this.data.delete(id);
    this.sizes.delete(id);
    this.dropped.delete(id);
    this.resets.get(id)?.forEach((l) => l());
  }

  private resets = new Map<string, Set<() => void>>();

  /// Told when the screen should be wiped under an open view.
  onReset(id: string, l: () => void): () => void {
    const set = this.resets.get(id) ?? new Set();
    set.add(l);
    this.resets.set(id, set);
    return () => set.delete(l);
  }

  subscribe(id: string, l: Listener): () => void {
    const set = this.listeners.get(id) ?? new Set();
    set.add(l);
    this.listeners.set(id, set);
    return () => set.delete(l);
  }
}

export const termBuffer = new TermBuffer();
