import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { api } from '../api';
import { termBuffer } from '../termstore';
import { terminalTheme } from '../themes';

/// A session's emulator, kept while the window is open. Leaving the terminal
/// for the chat, the code view or another session and coming back finds the
/// same one, not a new one fed the saved output again: that output is cut to
/// its last 2 MB, and with its start goes the part where Claude Code turned
/// on the alternate screen and mouse tracking — which is how its history
/// scrolls, so a new emulator could not scroll it.
interface Live {
  term: Terminal;
  fit: FitAddon;
  /// What xterm was opened in, moved into whichever view shows it.
  box: HTMLDivElement;
  dispose: () => void;
}
const live = new Map<string, Live>();

function create(id: string, host: HTMLElement): Live {
  const box = document.createElement('div');
  box.className = 'term-box';
  host.appendChild(box);
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
  term.open(box);
  try {
    fit.fit();
  } catch {
    // Not laid out yet.
  }

  // What came before the view first opened, then everything after it. The
  // terminal queries in the old part were answered when they came; answering
  // them again would type the replies into the program.
  let replaying = false;
  const past = termBuffer.chunks(id);
  if (past.length) {
    replaying = true;
    past.forEach((chunk, i) => term.write(chunk, i === past.length - 1 ? () => (replaying = false) : undefined));
  }
  const off = termBuffer.subscribe(id, (chunk) => term.write(chunk));
  const offReset = termBuffer.onReset(id, () => term.reset());
  const input = term.onData((d) => {
    if (!replaying) void api.termWrite(id, d);
  });
  const retheme = () => {
    term.options.theme = terminalTheme();
  };
  window.addEventListener('fleet-theme', retheme);
  const l: Live = {
    term,
    fit,
    box,
    dispose: () => {
      window.removeEventListener('fleet-theme', retheme);
      input.dispose();
      off();
      offReset();
      term.dispose();
      box.remove();
      live.delete(id);
    },
  };
  live.set(id, l);
  return l;
}

/// Let go of the emulators of sessions that are gone.
export function keepTerminals(ids: Set<string>) {
  for (const [id, l] of live) if (!ids.has(id)) l.dispose();
}

/// Claude Code's own screen, through a pseudoterminal on the Rust side.
export function TerminalView({ id }: { id: string }) {
  const host = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    let l = live.get(id);
    if (l) el.appendChild(l.box);
    else l = create(id, el);
    const { term, fit, box } = l;

    const sync = () => {
      try {
        fit.fit();
        void api.termResize(id, term.rows, term.cols);
      } catch {
        // Not laid out yet.
      }
    };
    sync();
    term.refresh(0, term.rows - 1);
    term.focus();
    const ro = new ResizeObserver(sync);
    ro.observe(el);
    return () => {
      ro.disconnect();
      if (box.parentElement === el) box.remove();
    };
  }, [id]);

  return <div className="term-host" ref={host} />;
}
