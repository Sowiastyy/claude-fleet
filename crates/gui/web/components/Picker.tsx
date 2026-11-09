import { useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { Check, ChevronDown } from 'lucide-react';

export interface Choice {
  value: string;
  label: string;
  /// Shown dim on the right: a model id, a path, a key.
  hint?: string;
  icon?: ReactNode;
  tone?: 'accent' | 'info' | 'danger';
  /// Starts a new group with a rule above it.
  divide?: boolean;
}

interface Props {
  label: string;
  value: string;
  choices: Choice[];
  onChange: (v: string) => void;
  /// What the button shows; the chosen option's label otherwise.
  display?: string;
  icon?: ReactNode;
  variant: 'pill' | 'foot';
  placement: 'up' | 'down';
  /// A line under the options, such as a shortcut.
  foot?: ReactNode;
}

/// A button that opens a list to choose from, worked by mouse or keys.
export function Picker(p: Props) {
  const [open, setOpen] = useState(false);
  const [hot, setHot] = useState(0);
  const wrap = useRef<HTMLDivElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const listId = useId();
  const chosen = p.choices.findIndex((c) => c.value === p.value);
  const current = p.choices[chosen];

  useEffect(() => {
    if (!open) return;
    setHot(Math.max(0, chosen));
    list.current?.focus();
    const away = (e: MouseEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', away);
    return () => document.removeEventListener('mousedown', away);
  }, [open]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (open) list.current?.querySelector<HTMLElement>(`[data-i="${hot}"]`)?.scrollIntoView({ block: 'nearest' });
  }, [hot, open]);

  const pick = (i: number) => {
    const c = p.choices[i];
    setOpen(false);
    button.current?.focus();
    if (c && c.value !== p.value) p.onChange(c.value);
  };

  const onKey = (e: React.KeyboardEvent) => {
    const n = p.choices.length;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        setHot((h) => (h + 1) % n);
        break;
      case 'ArrowUp':
        e.preventDefault();
        setHot((h) => (h - 1 + n) % n);
        break;
      case 'Home':
        e.preventDefault();
        setHot(0);
        break;
      case 'End':
        e.preventDefault();
        setHot(n - 1);
        break;
      case 'Enter':
      case ' ':
        e.preventDefault();
        pick(hot);
        break;
      case 'Escape':
        e.preventDefault();
        e.stopPropagation();
        setOpen(false);
        button.current?.focus();
        break;
      case 'Tab':
        setOpen(false);
        break;
      default:
        if (e.key.length === 1) {
          const k = e.key.toLowerCase();
          const at = p.choices.findIndex((c, i) => i > hot && c.label.toLowerCase().startsWith(k));
          const any = at >= 0 ? at : p.choices.findIndex((c) => c.label.toLowerCase().startsWith(k));
          if (any >= 0) setHot(any);
        }
    }
  };

  return (
    <div className={`picker ${p.variant}`} ref={wrap}>
      <button
        ref={button}
        type="button"
        className={`picker-btn ${current?.tone ? `tone-${current.tone}` : ''}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        aria-label={`${p.label}: ${p.display ?? current?.label ?? ''}`}
        onClick={() => setOpen(!open)}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            e.preventDefault();
            setOpen(true);
          }
        }}
      >
        {p.icon ?? current?.icon}
        <span className="picker-value">{p.display ?? current?.label}</span>
        <ChevronDown className="chev" size={13} aria-hidden />
      </button>
      {open && (
        <div className={`picker-pop ${p.placement}`}>
          <div className="picker-title">{p.label}</div>
          <ul
            id={listId}
            ref={list}
            role="listbox"
            tabIndex={-1}
            aria-label={p.label}
            aria-activedescendant={`${listId}-${hot}`}
            onKeyDown={onKey}
          >
            {p.choices.map((c, i) => (
              <li
                key={c.value}
                id={`${listId}-${i}`}
                data-i={i}
                role="option"
                aria-selected={i === chosen}
                className={`picker-opt ${i === hot ? 'hot' : ''} ${c.divide ? 'divide' : ''} ${c.tone ? `tone-${c.tone}` : ''}`}
                onMouseEnter={() => setHot(i)}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pick(i)}
              >
                <span className="picker-icon">{c.icon}</span>
                <span className="picker-label">{c.label}</span>
                {c.hint && <span className="picker-hint">{c.hint}</span>}
                <span className="picker-check">{i === chosen && <Check size={14} strokeWidth={2.5} aria-hidden />}</span>
              </li>
            ))}
          </ul>
          {p.foot && <div className="picker-foot">{p.foot}</div>}
        </div>
      )}
    </div>
  );
}
