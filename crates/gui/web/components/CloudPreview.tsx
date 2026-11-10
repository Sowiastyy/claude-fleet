import { useEffect, useRef } from 'react';
import { Cloud } from 'lucide-react';
import { api, errorText, type Session } from '../api';

// Calls to show and hide run side by side on the other end; the number
// tells it which came last.
let seq = 0;
const next = () => ++seq;

/// What opens over this box. The preview is a webview drawn above the whole
/// page, so while one of these overlaps it, the preview steps aside.
const COVERS = '.scrim, .modal-scrim, .palette-scrim, .lightbox, .picker-pop, .menu';

const overlaps = (a: DOMRect, b: DOMRect) =>
  a.width > 0 && a.height > 0 && a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;

interface Props {
  session: Session;
  /// Changed to go back to the session's own page.
  back: number;
  onError: (text: string) => void;
}

/// The cloud session's claude.ai page, here in the window: this box keeps
/// its place and the app lays the page over it.
export function CloudPreview({ session, back, onError }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const kick = useRef<(reload: boolean) => void>(() => {});
  const { account, cloudUrl } = session;

  useEffect(() => {
    const el = box.current;
    if (!el || !cloudUrl) return;
    let last = '';
    let frame = 0;
    let reload = false;
    let failed = false;
    const sync = () => {
      frame = 0;
      const r = el.getBoundingClientRect();
      const covered = [...document.querySelectorAll(COVERS)].some((c) => overlaps(c.getBoundingClientRect(), r));
      const b = { x: Math.round(r.left), y: Math.round(r.top), w: Math.round(r.width), h: Math.round(r.height) };
      const visible = !covered && b.w > 0 && b.h > 0;
      const key = visible ? `${b.x},${b.y},${b.w},${b.h}` : 'hidden';
      if (key === last && !reload) return;
      last = key;
      if (!visible) {
        void api.cloudViewHide(next());
        return;
      }
      const again = reload;
      reload = false;
      api.cloudViewShow(next(), account, cloudUrl, b, again).catch((e) => {
        if (failed) return;
        failed = true;
        onError(errorText(e));
      });
    };
    const later = () => {
      if (!frame) frame = requestAnimationFrame(sync);
    };
    kick.current = (again) => {
      reload = again;
      later();
    };
    const size = new ResizeObserver(later);
    size.observe(el);
    // Menus and dialogs come and go as elements.
    const page = new MutationObserver(later);
    page.observe(document.body, { childList: true, subtree: true });
    window.addEventListener('resize', later);
    later();
    return () => {
      kick.current = () => {};
      cancelAnimationFrame(frame);
      size.disconnect();
      page.disconnect();
      window.removeEventListener('resize', later);
      void api.cloudViewHide(next());
    };
  }, [account, cloudUrl]); // eslint-disable-line react-hooks/exhaustive-deps

  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    kick.current(true);
  }, [back]);

  return (
    <div className="preview-wrap" ref={box}>
      <div className="preview-empty">
        <Cloud size={22} aria-hidden />
        <span>Opening claude.ai</span>
        <span className="small faint">Each account signs in to claude.ai here once.</span>
      </div>
    </div>
  );
}
