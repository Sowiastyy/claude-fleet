import { forwardRef, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { ImagePlus, X } from 'lucide-react';
import type { Attachment } from '../api';
import { errorText } from '../api';
import { imageFiles, readImage } from '../attachments';

const MAX_IMAGES = 20;

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
  /// The row under the text: pickers and the send button.
  children: ReactNode;
  className?: string;
}

/// The message box: text, plus images attached by button, paste or drop.
export const Composer = forwardRef<HTMLTextAreaElement, Props>(function Composer(p, ref) {
  const [dropping, setDropping] = useState(false);
  const picker = useRef<HTMLInputElement>(null);
  const depth = useRef(0);
  const current = useRef(p.images);
  current.current = p.images;

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
        ref={ref}
        rows={p.rows}
        placeholder={p.placeholder}
        value={p.value}
        onChange={(e) => p.onChange(e.target.value)}
        onKeyDown={p.onKeyDown}
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
