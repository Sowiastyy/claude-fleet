import { forwardRef, useEffect, useImperativeHandle, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { FileText, ImagePlus, X } from 'lucide-react';
import type { Attachment } from '../api';
import { api, errorText } from '../api';
import { imageFiles, readImage } from '../attachments';

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

  const track = () => {
    const el = box.current;
    if (!el) return;
    setMention(p.cwd ? mentionAt(el.value, el.selectionStart) : null);
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

  const open = mention && matches.length > 0;

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
        }}
        onClick={track}
        onBlur={() => setMention(null)}
        onKeyDown={(e) => {
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
