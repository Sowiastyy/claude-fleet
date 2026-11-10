import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { CornerDownLeft, Ghost, RotateCcw, SkipForward } from 'lucide-react';
import { accuracy, course, matches, wpm, type GhostLine } from '../ghost';

interface Progress {
  pos: number;
  /// Cells where a wrong key was pressed first.
  errors: Set<number>;
  correct: number;
  mistakes: number;
  skipped: number;
  /// Time spent typing, not counting while the view was left.
  activeMs: number;
  resumedAt: number | null;
  done: boolean;
}

const fresh = (): Progress => ({ pos: 0, errors: new Set(), correct: 0, mistakes: 0, skipped: 0, activeMs: 0, resumedAt: null, done: false });

// Kept while switching tabs, so a long file can be typed in sittings.
const saved = new Map<string, Progress>();

const elapsed = (p: Progress, now: number) => p.activeMs + (p.resumedAt ? now - p.resumedAt : 0);

function fmtTime(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

interface Props {
  id: string;
  lines: GhostLine[];
  /// What is being typed, for the heading.
  label: string;
  truncated: boolean;
  onClose: () => void;
}

export function GhostView({ id, lines, label, truncated, onClose }: Props) {
  const [typeIndent, setTypeIndent] = useState(() => localStorage.getItem('fleet.ghost.indent') === '1');
  const c = useMemo(() => course(lines, typeIndent), [lines, typeIndent]);
  const [p, setP] = useState<Progress>(() => saved.get(id) ?? fresh());
  const [focused, setFocused] = useState(false);
  const [miss, setMiss] = useState(0);
  const [, tick] = useState(0);
  const input = useRef<HTMLTextAreaElement>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const total = c.cells.length;

  // Several keys can arrive in one input event; each must see the last.
  const latest = useRef(p);
  const update = (f: (p: Progress) => Progress) => {
    const n = f(latest.current);
    if (n === latest.current) return;
    latest.current = n;
    saved.set(id, n);
    setP(n);
  };

  useEffect(() => {
    if (!p.resumedAt) return;
    const t = window.setInterval(() => tick((n) => n + 1), 1000);
    return () => window.clearInterval(t);
  }, [p.resumedAt]);

  const press = (ch: string) =>
    update((o) => {
      if (o.done || o.pos >= total) return o;
      const now = Date.now();
      const resumedAt = o.resumedAt ?? now;
      if (matches(c.cells[o.pos].ch, ch)) {
        const pos = o.pos + 1;
        if (pos >= total) return { ...o, pos, correct: o.correct + 1, done: true, activeMs: o.activeMs + (now - resumedAt), resumedAt: null };
        return { ...o, pos, correct: o.correct + 1, resumedAt };
      }
      setMiss((n) => n + 1);
      const errors = new Set(o.errors).add(o.pos);
      return { ...o, errors, mistakes: o.mistakes + 1, resumedAt };
    });

  const pause = () => update((o) => (o.resumedAt ? { ...o, activeMs: o.activeMs + (Date.now() - o.resumedAt), resumedAt: null } : o));

  const skipLine = () =>
    update((o) => {
      if (o.done || o.pos >= total) return o;
      const line = c.cells[o.pos].line;
      let pos = o.pos;
      while (pos < total && c.cells[pos].line === line) pos++;
      const done = pos >= total;
      return { ...o, pos, skipped: o.skipped + (pos - o.pos), done, ...(done ? { activeMs: elapsed(o, Date.now()), resumedAt: null } : {}) };
    });

  const restart = () => {
    update(() => fresh());
    window.setTimeout(() => input.current?.focus(), 0);
  };

  const toggleIndent = () => {
    localStorage.setItem('fleet.ghost.indent', typeIndent ? '0' : '1');
    setTypeIndent(!typeIndent);
    update(() => fresh());
    input.current?.focus();
  };

  // Keep the line being typed in the middle third of the view.
  const curLine = p.pos < total ? c.cells[p.pos].line : -1;
  useLayoutEffect(() => {
    const box = scroller.current;
    if (!box || curLine < 0) return;
    const el = box.querySelector<HTMLElement>(`[data-line="${curLine}"]`);
    if (!el) return;
    const top = el.offsetTop - box.offsetTop;
    if (top < box.scrollTop + box.clientHeight / 3 || top > box.scrollTop + (box.clientHeight * 2) / 3)
      box.scrollTo({ top: top - box.clientHeight / 2.5, behavior: 'smooth' });
  }, [curLine]);

  useEffect(() => {
    input.current?.focus();
    return () => {
      const o = saved.get(id);
      if (o?.resumedAt) saved.set(id, { ...o, activeMs: elapsed(o, Date.now()), resumedAt: null });
    };
  }, [id]);

  const ms = elapsed(p, Date.now());
  const speed = wpm(p.correct, ms);
  const acc = accuracy(p.correct, p.mistakes);
  const pct = total ? Math.floor((p.pos / total) * 100) : 100;
  // Each row is told only its own mistakes, so the others skip re-rendering.
  const errsByLine = useMemo(() => {
    const m = new Map<number, string>();
    for (const k of p.errors) {
      const line = c.cells[k]?.line;
      if (line !== undefined) m.set(line, `${m.get(line) ?? ''}${k},`);
    }
    return m;
  }, [p.errors, c]);

  return (
    <div className="ghost" onMouseDown={(e) => {
      if (!(e.target as HTMLElement).closest('button')) {
        e.preventDefault();
        input.current?.focus();
      }
    }}>
      <div className="ghost-bar">
        <Ghost size={15} aria-hidden color="var(--accent)" />
        <span className="ghost-label ellipsis small" title={label}>{label}</span>
        <div className="ghost-meter" title={`${pct}% typed`}>
          <div style={{ width: `${pct}%` }} />
        </div>
        <span className="ghost-stat" title="Typed"><b>{pct}%</b></span>
        <span className="ghost-stat" title="Words per minute"><b>{speed}</b> wpm</span>
        <span className="ghost-stat" title="Accuracy"><b>{acc}%</b> acc</span>
        <span className="ghost-stat" title="Wrong keys"><b>{p.mistakes}</b> err</span>
        <span className="ghost-stat" title="Time spent typing"><b>{fmtTime(ms)}</b></span>
        <div style={{ flex: 1 }} />
        <button type="button" className={`btn sm ${typeIndent ? 'on' : ''}`} aria-pressed={typeIndent} onClick={toggleIndent} title="Type the indentation too, or start each line at its code">
          ⇥ Indent
        </button>
        <button type="button" className="icon-btn boxed sm" onClick={skipLine} title="Skip the rest of this line" aria-label="Skip line">
          <SkipForward size={13} aria-hidden />
        </button>
        <button type="button" className="icon-btn boxed sm" onClick={restart} title="Start over" aria-label="Restart">
          <RotateCcw size={13} aria-hidden />
        </button>
      </div>
      <textarea
        ref={input}
        className="ghost-input"
        aria-label="Type the code here"
        autoCapitalize="off"
        autoComplete="off"
        autoCorrect="off"
        spellCheck={false}
        onFocus={() => setFocused(true)}
        onBlur={() => {
          setFocused(false);
          pause();
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            press('\n');
          } else if (e.key === 'Tab') {
            e.preventDefault();
            press('\t');
          } else if (e.key === 'Backspace' || e.key === 'Delete') {
            e.preventDefault();
          } else if (e.key === 'Escape') {
            input.current?.blur();
          }
        }}
        onInput={(e) => {
          const el = e.currentTarget;
          if ((e.nativeEvent as InputEvent).isComposing) return;
          const text = el.value;
          el.value = '';
          for (const ch of text) press(ch === '\r' ? '\n' : ch);
        }}
      />
      <div className={`ghost-code ${focused ? 'focused' : ''}`} ref={scroller}>
        {c.lines.map((l, i) => {
          const [first, end] = c.ranges[i];
          const cur = p.pos < first ? -1 : p.pos >= end ? Infinity : p.pos;
          const errs = errsByLine.get(i) ?? '';
          return (
            <GhostRow
              key={i}
              index={i}
              line={l}
              first={first}
              end={end}
              start={end > first ? c.cells[first].col : 0}
              nl={end > first && c.cells[end - 1].ch === '\n'}
              cur={cur}
              errs={errs}
              missKey={cur !== -1 && cur !== Infinity ? miss : 0}
            />
          );
        })}
        {truncated && <div className="ghost-row gap">⋯ the rest is too long for one sitting</div>}
        <div style={{ height: '40vh' }} />
      </div>
      {!focused && !p.done && (
        <div className="ghost-hint">
          {p.pos === 0 ? 'Click and start typing — the faint code is what to type' : 'Paused — click to go on'}
          <span className="faint small"> Enter ends a line · indentation is filled in</span>
        </div>
      )}
      {p.done && (
        <div className="ghost-done" role="dialog" aria-label="Done">
          <Ghost size={26} color="var(--accent)" aria-hidden />
          <h3>All typed</h3>
          <div className="ghost-done-stats">
            <span><b>{speed}</b> wpm</span>
            <span><b>{acc}%</b> accuracy</span>
            <span><b>{p.mistakes}</b> mistakes</span>
            <span><b>{fmtTime(ms)}</b> time</span>
            {p.skipped > 0 && <span><b>{p.skipped}</b> skipped</span>}
          </div>
          <div className="btn-row">
            <button type="button" className="btn primary" onClick={restart}>
              <RotateCcw size={13} aria-hidden /> Again
            </button>
            <button type="button" className="btn" onClick={onClose}>
              Close
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

const GhostRow = memo(function GhostRow(p: {
  index: number;
  line: GhostLine;
  first: number;
  end: number;
  /// The column of the first key on the line.
  start: number;
  /// The line's last key is Enter.
  nl: boolean;
  /// The key being typed, when it is on this line; -1 before, Infinity after.
  cur: number;
  errs: string;
  /// Changes on each wrong key, to replay the shake.
  missKey: number;
}) {
  const { line, first, end, start, cur } = p;
  const no = <span className="ghost-no">{line.no ?? ''}</span>;
  if (line.kind === 'gap') return <div className="ghost-row gap">⋯</div>;
  if (line.kind === 'given' || end === first)
    return (
      <div className={`ghost-row ${line.kind === 'given' ? 'given' : ''}`} data-line={p.index}>
        {no}
        <span className="ghost-text">{line.text || ' '}</span>
      </div>
    );

  const errs = new Set(p.errs ? p.errs.split(',').filter(Boolean).map(Number) : []);
  const contentEnd = p.nl ? end - 1 : end;
  const parts: Array<{ cls: string; text: string }> = [];
  const push = (cls: string, text: string) => {
    const last = parts[parts.length - 1];
    if (last && last.cls === cls) last.text += text;
    else parts.push({ cls, text });
  };
  // Indentation: filled in once the line is reached, as an editor would.
  if (start > 0) push(cur === -1 ? 'todo' : 'done', line.text.slice(0, start));
  for (let k = first; k < contentEnd; k++) {
    const ch = line.text[start + (k - first)];
    if (k < cur) push(errs.has(k) ? 'done fixed' : 'done', ch);
    else if (k === cur) push(errs.has(k) ? 'cur wrong' : 'cur', ch);
    else push('todo', ch);
  }
  const atEnd = p.nl && cur === end - 1;
  const active = cur !== -1 && cur !== Infinity;
  return (
    <div className={`ghost-row to-type ${active ? 'active' : ''} ${cur === Infinity ? 'typed' : ''}`} data-line={p.index}>
      {no}
      <span className="ghost-text">
        {parts.map((s, i) =>
          s.cls.startsWith('cur') ? (
            <span key={`${i}-${p.missKey}`} className={`g-${s.cls.replace(' ', ' g-')}`}>
              {s.text === '\t' ? '\t' : s.text}
            </span>
          ) : (
            <span key={i} className={s.cls.split(' ').map((x) => `g-${x}`).join(' ')}>
              {s.text}
            </span>
          ),
        )}
        {p.nl && (
          <span key={`nl-${p.missKey}`} className={`g-nl ${atEnd ? `g-cur${errs.has(end - 1) ? ' g-wrong' : ''}` : cur > end - 1 ? 'g-done' : 'g-todo'}`}>
            <CornerDownLeft size={11} aria-hidden />
          </span>
        )}
        {line.text.slice(line.text.trimEnd().length)}
      </span>
    </div>
  );
});
