import { forwardRef, useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { FileText, ImagePlus, SquareSlash, SquareTerminal, X } from 'lucide-react';
import type { Attachment, SlashCommand } from '../api';
import { api, errorText } from '../api';
import { imageFiles, readImage } from '../attachments';
import { rankCommands, slashAt } from '../commands';

const MAX_IMAGES = 20;
const SHOWN = 8;

interface Props {
  id: string;
  label: string;
  placeholder: string;
  rows: number;
  value: string;
  onChange: (v: string) => void;
  images: Attachment[];
  onImages: (a: Attachment[]) => void;
  onKeyDown: (e: KeyboardEvent<HTMLTextAreaElement>) => void;
  onError: (text: string) => void;
  /// The folder `@` offers files from.
  cwd: string;
  /// The row under the text: pickers and the send button.
  children: ReactNode;
  className?: string;
  /// What `/` offers; none, no menu.
  commands?: SlashCommand[];
  /// Send this text now: a command picked from the menu that takes nothing
  /// after it.
  onSubmit?: (text: string) => void;
}

// Each folder's file list, kept for a while: listing a big repo is not free.
const fileCache = new Map<string, { at: number; files: Promise<string[]> }>();
function filesOf(cwd: string): Promise<string[]> {
  const hit = fileCache.get(cwd);
  if (hit && Date.now() - hit.at < 30_000) return hit.files;
  const files = api.files(cwd).catch(() => [] as string[]);
  fileCache.set(cwd, { at: Date.now(), files });
  return files;
}

/// Files that match what follows `@`: the name first, then the path, then
/// the letters in order anywhere.
function rank(files: string[], query: string): string[] {
  if (!query) return files.slice(0, SHOWN);
  const q = query.toLowerCase().replace(/\\/g, '/');
  const scored: Array<[number, string]> = [];
  for (const f of files) {
    const p = f.toLowerCase();
    const base = p.slice(p.lastIndexOf('/') + 1);
    let score = -1;
    if (base.startsWith(q)) score = 0;
    else if (base.includes(q)) score = 1;
    else if (p.includes(q)) score = 2;
    else {
      let i = 0;
      for (const ch of p) if (ch === q[i]) i += 1;
      if (i === q.length) score = 3;
    }
    if (score >= 0) scored.push([score * 1000 + p.length, f]);
  }
  scored.sort((a, b) => a[0] - b[0]);
  return scored.slice(0, SHOWN).map(([, f]) => f);
}

/// The `@word` the caret sits at the end of, if any.
function mentionAt(text: string, caret: number): { start: number; query: string } | null {
  const m = /(^|\s)@([^\s@]*)$/.exec(text.slice(0, caret));
  return m ? { start: caret - m[2].length - 1, query: m[2] } : null;
}

/// The message box: text, plus images attached by button, paste or drop,
/// and files named with `@`.
export const Composer = forwardRef<HTMLTextAreaElement, Props>(function Composer(p, ref) {
  const [dropping, setDropping] = useState(false);
  const [mention, setMention] = useState<{ start: number; query: string } | null>(null);
  const [matches, setMatches] = useState<string[]>([]);
  const [hot, setHot] = useState(0);
  const [slash, setSlash] = useState<string | null>(null);
  const [slashHot, setSlashHot] = useState(0);
  const slashList = useRef<HTMLUListElement>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  useImperativeHandle(ref, () => box.current!, []);
  const picker = useRef<HTMLInputElement>(null);
  const depth = useRef(0);
  const current = useRef(p.images);
  current.current = p.images;

  // Grow with the text, up to a point.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, Math.round(window.innerHeight * 0.4))}px`;
  }, [p.value]);

  useEffect(() => {
    if (!mention) return;
    let live = true;
    void filesOf(p.cwd).then((files) => {
      if (!live) return;
      setMatches(rank(files, mention.query));
      setHot(0);
    });
    return () => {
      live = false;
    };
  }, [mention?.query, mention?.start, p.cwd]); // eslint-disable-line react-hooks/exhaustive-deps

  const commandMatches = useMemo(
    () => (slash === null || !p.commands?.length ? [] : rankCommands(p.commands, slash).slice(0, 60)),
    [slash, p.commands],
  );
  useEffect(() => setSlashHot(0), [slash]);
  // Sent or cleared from outside: the menu goes with the text.
  useEffect(() => {
    if (!p.value.startsWith('/')) setSlash(null);
  }, [p.value]);
  useEffect(() => {
    slashList.current?.querySelector<HTMLElement>(`[data-i="${slashHot}"]`)?.scrollIntoView({ block: 'nearest' });
  }, [slashHot]);

  const track = () => {
    const el = box.current;
    if (!el) return;
    setMention(p.cwd ? mentionAt(el.value, el.selectionStart) : null);
    setSlash(slashAt(el.value, el.selectionStart));
  };

  /// Put the picked command in place of the word being typed; one that
  /// takes nothing after it is sent at once when `run`.
  const pick = (c: SlashCommand, run: boolean) => {
    const el = box.current;
    if (!el) return;
    const rest = p.value.replace(/^\/\S*/, '').replace(/^\s+/, '');
    setSlash(null);
    if (run && !c.argumentHint && !rest && p.onSubmit) {
      p.onSubmit(`/${c.name}`);
      return;
    }
    const next = `/${c.name} ${rest}`;
    const at = c.name.length + 2;
    p.onChange(next);
    requestAnimationFrame(() => {
      el.focus();
      el.setSelectionRange(at, at);
    });
  };

  const insert = (path: string) => {
    const el = box.current;
    if (!el || !mention) return;
    const caret = el.selectionStart;
    const quoted = /\s/.test(path) ? `"${path}"` : path;
    const next = `${p.value.slice(0, mention.start)}@${quoted} ${p.value.slice(caret)}`;
    const at = mention.start + quoted.length + 2;
    p.onChange(next);
    setMention(null);
    requestAnimationFrame(() => {
      el.focus();
      el.setSelectionRange(at, at);
    });
  };

  const add = async (files: File[]) => {
    if (!files.length) return;
    const room = MAX_IMAGES - current.current.length;
    if (room <= 0) {
      p.onError(`At most ${MAX_IMAGES} images`);
      return;
    }
    const read: Attachment[] = [];
    for (const f of files.slice(0, room)) {
      try {
        read.push(await readImage(f, f.name || 'pasted image'));
      } catch (e) {
        p.onError(errorText(e));
      }
    }
    if (read.length) p.onImages([...current.current, ...read]);
  };

  const slashOpen = commandMatches.length > 0;
  const open = !slashOpen && mention && matches.length > 0;

  return (
    <div
      className={`composer ${p.className ?? ''} ${dropping ? 'dropping' : ''}`}
      onDragEnter={(e) => {
        if (!Array.from(e.dataTransfer.types).includes('Files')) return;
        depth.current += 1;
        setDropping(true);
      }}
      onDragOver={(e) => {
        if (Array.from(e.dataTransfer.types).includes('Files')) {
          e.preventDefault();
          e.dataTransfer.dropEffect = 'copy';
        }
      }}
      onDragLeave={() => {
        depth.current = Math.max(0, depth.current - 1);
        if (!depth.current) setDropping(false);
      }}
      onDrop={(e) => {
        e.preventDefault();
        depth.current = 0;
        setDropping(false);
        void add(imageFiles(e.dataTransfer.files));
      }}
    >
      {slashOpen && (
        <ul className="mentions commands-menu" role="listbox" aria-label="Commands" ref={slashList}>
          {commandMatches.map((c, i) => (
            <li
              key={c.name}
              data-i={i}
              role="option"
              aria-selected={i === slashHot}
              className={i === slashHot ? 'hot' : ''}
              onMouseEnter={() => setSlashHot(i)}
              onMouseDown={(e) => {
                e.preventDefault();
                pick(c, true);
              }}
              title={c.description}
            >
              {c.terminal ? <SquareTerminal size={14} aria-hidden /> : <SquareSlash size={14} aria-hidden />}
              <span className="cmd-name">/{c.name}</span>
              {c.argumentHint && <span className="cmd-hint">{c.argumentHint}</span>}
              <span className="cmd-desc">{c.description}</span>
              {c.terminal && <span className="cmd-tag">terminal</span>}
            </li>
          ))}
        </ul>
      )}
      {open && (
        <ul className="mentions" role="listbox" aria-label="Files">
          {matches.map((f, i) => {
            const cut = f.lastIndexOf('/');
            return (
              <li
                key={f}
                role="option"
                aria-selected={i === hot}
                className={i === hot ? 'hot' : ''}
                onMouseEnter={() => setHot(i)}
                onMouseDown={(e) => {
                  e.preventDefault();
                  insert(f);
                }}
              >
                <FileText size={14} aria-hidden />
                <span className="mention-name">{f.slice(cut + 1)}</span>
                <span className="mention-dir">{cut > 0 ? f.slice(0, cut) : ''}</span>
              </li>
            );
          })}
        </ul>
      )}
      {p.images.length > 0 && (
        <ul className="attachments" aria-label="Images">
          {p.images.map((a) => (
            <li key={a.id} className="attachment">
              <img src={a.url} alt={a.name} />
              <button
                type="button"
                className="attachment-x"
                aria-label={`Remove ${a.name}`}
                onClick={() => p.onImages(p.images.filter((x) => x.id !== a.id))}
              >
                <X size={12} strokeWidth={2.5} aria-hidden />
              </button>
            </li>
          ))}
        </ul>
      )}
      <label htmlFor={p.id} className="sr-only">{p.label}</label>
      <textarea
        id={p.id}
        ref={box}
        rows={p.rows}
        placeholder={p.placeholder}
        value={p.value}
        onChange={(e) => {
          p.onChange(e.target.value);
          setMention(p.cwd ? mentionAt(e.target.value, e.target.selectionStart) : null);
          setSlash(slashAt(e.target.value, e.target.selectionStart));
        }}
        onClick={track}
        onBlur={() => {
          setMention(null);
          setSlash(null);
        }}
        onKeyDown={(e) => {
          if (slashOpen) {
            if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
              e.preventDefault();
              const n = commandMatches.length;
              setSlashHot((h) => (h + (e.key === 'ArrowDown' ? 1 : n - 1)) % n);
              return;
            }
            if ((e.key === 'Enter' && !e.shiftKey) || e.key === 'Tab') {
              const c = commandMatches[slashHot];
              // Enter on a command already typed in full sends it as it is.
              if (!(e.key === 'Enter' && slash === c.name)) {
                e.preventDefault();
                pick(c, e.key === 'Enter');
                return;
              }
            }
            if (e.key === 'Escape') {
              e.preventDefault();
              e.stopPropagation();
              setSlash(null);
              return;
            }
          }
          if (open) {
            if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
              e.preventDefault();
              const n = matches.length;
              setHot((h) => (h + (e.key === 'ArrowDown' ? 1 : n - 1)) % n);
              return;
            }
            if (e.key === 'Enter' || e.key === 'Tab') {
              e.preventDefault();
              insert(matches[hot]);
              return;
            }
            if (e.key === 'Escape') {
              e.preventDefault();
              e.stopPropagation();
              setMention(null);
              return;
            }
          }
          p.onKeyDown(e);
        }}
        onKeyUp={(e) => {
          if (e.key === 'ArrowLeft' || e.key === 'ArrowRight' || e.key === 'Home' || e.key === 'End') track();
        }}
        onPaste={(e) => {
          const files = imageFiles(e.clipboardData?.items);
          if (files.length) {
            e.preventDefault();
            void add(files);
          }
        }}
      />
      <div className="composer-row">
        <button type="button" className="icon-btn" title="Attach image" aria-label="Attach image" onClick={() => picker.current?.click()}>
          <ImagePlus size={17} aria-hidden />
        </button>
        <input
          ref={picker}
          type="file"
          accept="image/*"
          multiple
          hidden
          onChange={(e) => {
            void add(imageFiles(e.target.files));
            e.target.value = '';
          }}
        />
        {p.children}
      </div>
      {dropping && (
        <div className="drop-hint" aria-hidden>
          <ImagePlus size={20} />
          Drop images
        </div>
      )}
    </div>
  );
});
