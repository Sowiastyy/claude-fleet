import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { api } from '../api';
import { termBuffer } from '../termstore';
import { terminalTheme } from '../themes';

/// Claude Code's own screen, through a pseudoterminal on the Rust side.
export function TerminalView({ id }: { id: string }) {
  const host = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    const term = new Terminal({
      fontFamily: "'IBM Plex Mono', ui-monospace, monospace",
      fontSize: 13,
      lineHeight: 1.2,
      cursorBlink: true,
      allowTransparency: true,
      scrollback: 5000,
      theme: terminalTheme(),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);

    const sync = () => {
      try {
        fit.fit();
        void api.termResize(id, term.rows, term.cols);
      } catch {
        // Not laid out yet.
      }
    };
    sync();
    term.focus();

    // What came before the view opened, then everything after it.
    for (const chunk of termBuffer.chunks(id)) term.write(chunk);
    const off = termBuffer.subscribe(id, (chunk) => term.write(chunk));
    const offReset = termBuffer.onReset(id, () => term.reset());
    const input = term.onData((d) => void api.termWrite(id, d));
    const retheme = () => {
      term.options.theme = terminalTheme();
    };
    window.addEventListener('fleet-theme', retheme);
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    return () => {
      ro.disconnect();
      window.removeEventListener('fleet-theme', retheme);
      input.dispose();
      off();
      offReset();
      term.dispose();
    };
  }, [id]);

  return <div className="term-host" ref={host} />;
}
