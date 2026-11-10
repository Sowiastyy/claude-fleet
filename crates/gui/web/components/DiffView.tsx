import { memo, useEffect, useMemo, useState } from 'react';
import { api, errorText, type DiffOf } from '../api';

interface Row {
  kind: 'add' | 'del' | 'ctx' | 'hunk' | 'meta' | 'note';
  text: string;
  old: number | null;
  new: number | null;
}

/// A unified diff as rows with both sides' line numbers.
export function rows(lines: string[]): Row[] {
  const out: Row[] = [];
  let o = 0;
  let n = 0;
  let inHunk = false;
  for (const l of lines) {
    const h = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/.exec(l);
    if (h) {
      o = Number(h[1]);
      n = Number(h[2]);
      inHunk = true;
      out.push({ kind: 'hunk', text: l, old: null, new: null });
      continue;
    }
    if (inHunk && l.startsWith('+')) out.push({ kind: 'add', text: l.slice(1), old: null, new: n++ });
    else if (inHunk && l.startsWith('-')) out.push({ kind: 'del', text: l.slice(1), old: o++, new: null });
    else if (inHunk && l.startsWith(' ')) out.push({ kind: 'ctx', text: l.slice(1), old: o++, new: n++ });
    else if (inHunk && l.startsWith('\\')) out.push({ kind: 'note', text: l, old: null, new: null });
    else if (!inHunk && l.startsWith('+') && !l.startsWith('+++')) out.push({ kind: 'add', text: l.slice(1), old: null, new: ++n });
    else {
      inHunk = false;
      out.push({ kind: /^(diff |index |--- |\+\+\+ |new file|deleted file|similarity|rename |old mode|new mode)/.test(l) ? 'meta' : 'note', text: l, old: null, new: null });
    }
  }
  return out;
}

/// What a commit did to a file, or what is uncommitted in it.
export function DiffView({ base, of, onLines }: { base: string; of: DiffOf; onLines?: (lines: string[]) => void }) {
  const [lines, setLines] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const key = JSON.stringify(of);

  useEffect(() => {
    let live = true;
    setLines(null);
    setError(null);
    api
      .diff(base, of)
      .then((l) => {
        if (!live) return;
        setLines(l);
        onLines?.(l);
      })
      .catch((e) => live && setError(errorText(e)));
    return () => {
      live = false;
    };
  }, [base, key]); // eslint-disable-line react-hooks/exhaustive-deps

  const parsed = useMemo(() => (lines ? rows(lines) : []), [lines]);
  const added = parsed.filter((r) => r.kind === 'add').length;
  const removed = parsed.filter((r) => r.kind === 'del').length;

  if (error) return <div className="bench-empty">{error}</div>;
  if (!lines) return <div className="bench-empty">Reading the diff…</div>;
  return (
    <div className="cdiff">
      <div className="cdiff-sum small">
        <span className="plus">+{added}</span> <span className="minus">−{removed}</span>
      </div>
      <div className="cdiff-rows">
        {parsed.map((r, i) => (
          <DiffRow key={i} r={r} />
        ))}
      </div>
    </div>
  );
}

const DiffRow = memo(function DiffRow({ r }: { r: Row }) {
  if (r.kind === 'hunk' || r.kind === 'meta' || r.kind === 'note')
    return <div className={`cdiff-row ${r.kind}`}>{r.text || ' '}</div>;
  return (
    <div className={`cdiff-row ${r.kind}`}>
      <span className="ln">{r.old ?? ''}</span>
      <span className="ln">{r.new ?? ''}</span>
      <span className="sign">{r.kind === 'add' ? '+' : r.kind === 'del' ? '−' : ' '}</span>
      <span className="txt">{r.text || ' '}</span>
    </div>
  );
});
