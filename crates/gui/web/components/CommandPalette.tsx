import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { Search } from 'lucide-react';

export interface Command {
  id: string;
  group: string;
  label: string;
  hint?: string;
  /// Searched but not shown: a conversation's id, its folder's path.
  keys?: string;
  icon?: ReactNode;
  run: () => void;
}

/// Every word typed must appear somewhere in the label, group or hint;
/// labels that start with the first word come first.
function filter(commands: Command[], query: string): Command[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return commands;
  const hits = commands.filter((c) => {
    const hay = `${c.label} ${c.group} ${c.hint ?? ''} ${c.keys ?? ''}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
  const first = (c: Command) => (c.label.toLowerCase().startsWith(words[0]) ? 0 : 1);
  return hits.sort((a, b) => first(a) - first(b));
}

interface Props {
  commands: Command[];
  onClose: () => void;
  /// What the dialog is for, to a screen reader and in the empty box.
  label?: string;
  placeholder?: string;
  /// Typed in already: what followed `/resume`.
  initialQuery?: string;
  /// Shown while `commands` is still being read.
  loading?: boolean;
}

export function CommandPalette({ commands, onClose, label = 'Commands', placeholder, initialQuery = '', loading }: Props) {
  const [query, setQuery] = useState(initialQuery);
  const [hot, setHot] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const shown = useMemo(() => filter(commands, query), [commands, query]);

  useEffect(() => input.current?.focus(), []);
  useEffect(() => setHot(0), [query]);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-i="${hot}"]`)?.scrollIntoView({ block: 'nearest' });
  }, [hot]);

  const run = (c: Command | undefined) => {
    if (!c) return;
    onClose();
    c.run();
  };

  let lastGroup = '';
  return (
    <div className="palette-scrim" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="palette" role="dialog" aria-label={label}>
        <div className="palette-input">
          <Search size={16} aria-hidden />
          <input
            ref={input}
            value={query}
            placeholder={placeholder ?? 'Search sessions, conversations, commands'}
            aria-label={label}
            aria-controls="palette-list"
            aria-activedescendant={shown[hot] ? `cmd-${shown[hot].id}` : undefined}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'ArrowDown') {
                e.preventDefault();
                setHot((h) => Math.min(shown.length - 1, h + 1));
              } else if (e.key === 'ArrowUp') {
                e.preventDefault();
                setHot((h) => Math.max(0, h - 1));
              } else if (e.key === 'Enter') {
                e.preventDefault();
                run(shown[hot]);
              } else if (e.key === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                onClose();
              }
            }}
          />
          <kbd>Esc</kbd>
        </div>
        <ul id="palette-list" ref={list} role="listbox" aria-label={label}>
          {shown.map((c, i) => {
            const head = c.group !== lastGroup ? c.group : null;
            lastGroup = c.group;
            return (
              <li key={c.id} role="presentation">
                {head && <div className="palette-group">{head}</div>}
                <div
                  id={`cmd-${c.id}`}
                  data-i={i}
                  role="option"
                  aria-selected={i === hot}
                  className={`palette-item ${i === hot ? 'hot' : ''}`}
                  onMouseMove={() => setHot(i)}
                  onClick={() => run(c)}
                >
                  <span className="palette-icon">{c.icon}</span>
                  <span className="palette-label">{c.label}</span>
                  {c.hint && <span className="palette-hint">{c.hint}</span>}
                </div>
              </li>
            );
          })}
          {!shown.length && <li className="palette-empty">{loading ? 'Reading conversations…' : 'No match'}</li>}
        </ul>
      </div>
    </div>
  );
}
