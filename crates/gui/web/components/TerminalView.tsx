import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { api } from '../api';
import { termBuffer } from '../termstore';

const THEME = {
  background: 'rgba(0,0,0,0)',
  foreground: '#e6ebf2',
  cursor: '#e2b867',
  cursorAccent: '#0d131d',
  selectionBackground: 'rgba(127,178,255,0.35)',
  black: '#0d131d',
  red: '#f07c70',
  green: '#59c48f',
  yellow: '#f0b54a',
  blue: '#7fb2ff',
  magenta: '#c9a7ff',
  cyan: '#6fd3d0',
  white: '#c5cfdc',
  brightBlack: '#7c8aa0',
  brightRed: '#ff9b90',
  brightGreen: '#7fdcaa',
  brightYellow: '#f5cd7a',
  brightBlue: '#a3c8ff',
  brightMagenta: '#dcc4ff',
  brightCyan: '#95e3e0',
  brightWhite: '#ffffff',
};

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
      theme: THEME,
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
    const input = term.onData((d) => void api.termWrite(id, d));
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    return () => {
      ro.disconnect();
      input.dispose();
      off();
      term.dispose();
    };
  }, [id]);

  return <div className="term-host" ref={host} />;
}
