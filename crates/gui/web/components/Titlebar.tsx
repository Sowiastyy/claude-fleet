import { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Copy, Minus, Sailboat, Square, X } from 'lucide-react';

interface Props {
  title: string;
  version?: string;
}

/// The window's own title bar: the native one is off (`decorations: false`)
/// so this one can match the app.
export function Titlebar({ title, version }: Props) {
  const win = getCurrentWindow();
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    let live = true;
    const read = () => void win.isMaximized().then((m) => live && setMaximized(m)).catch(() => {});
    read();
    const off = win.onResized(read).catch(() => () => {});
    return () => {
      live = false;
      void off.then((f) => f());
    };
  }, [win]);

  return (
    <header className="titlebar" data-tauri-drag-region>
      <div className="titlebar-brand" data-tauri-drag-region>
        <Sailboat size={16} color="var(--accent)" aria-hidden />
        <span className="titlebar-name">Fleet</span>
        {version && (
          <span className="titlebar-version" title={`Claude Fleet ${version}`}>
            v{version}
          </span>
        )}
      </div>
      <div className="titlebar-title ellipsis" data-tauri-drag-region>
        {title}
      </div>
      <div className="window-controls">
        <button type="button" aria-label="Minimize" onClick={() => void win.minimize()}>
          <Minus size={16} strokeWidth={1.5} aria-hidden />
        </button>
        <button type="button" aria-label={maximized ? 'Restore' : 'Maximize'} onClick={() => void win.toggleMaximize()}>
          {maximized ? <Copy size={13} strokeWidth={1.5} style={{ transform: 'scaleX(-1)' }} aria-hidden /> : <Square size={13} strokeWidth={1.5} aria-hidden />}
        </button>
        <button type="button" className="close" aria-label="Close" onClick={() => void win.close()}>
          <X size={17} strokeWidth={1.5} aria-hidden />
        </button>
      </div>
    </header>
  );
}
