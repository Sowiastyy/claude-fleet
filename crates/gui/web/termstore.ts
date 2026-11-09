// Terminal output kept outside React: it arrives many times a second and a
// view that opens later has to replay what it missed.
type Listener = (chunk: Uint8Array) => void;

const LIMIT = 2 * 1024 * 1024;

class TermBuffer {
  private data = new Map<string, Uint8Array[]>();
  private sizes = new Map<string, number>();
  private listeners = new Map<string, Set<Listener>>();

  push(id: string, b64: string) {
    const bin = atob(b64);
    const chunk = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) chunk[i] = bin.charCodeAt(i);
    const list = this.data.get(id) ?? [];
    list.push(chunk);
    let size = (this.sizes.get(id) ?? 0) + chunk.length;
    while (size > LIMIT && list.length > 1) size -= list.shift()!.length;
    this.data.set(id, list);
    this.sizes.set(id, size);
    this.listeners.get(id)?.forEach((l) => l(chunk));
  }

  chunks(id: string): Uint8Array[] {
    return this.data.get(id) ?? [];
  }

  /// A new process starts on a clean screen.
  reset(id: string) {
    this.data.delete(id);
    this.sizes.delete(id);
  }

  subscribe(id: string, l: Listener): () => void {
    const set = this.listeners.get(id) ?? new Set();
    set.add(l);
    this.listeners.set(id, set);
    return () => set.delete(l);
  }
}

export const termBuffer = new TermBuffer();
