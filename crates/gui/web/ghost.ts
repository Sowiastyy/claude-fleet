// Ghost mode: code shown faint, typed over by hand, one character at a time.
// What to type comes from a whole file, a selection, or the lines a change
// added — with the lines around them shown for context but not typed.

export type GhostKind = 'type' | 'given' | 'gap';

export interface GhostLine {
  text: string;
  kind: GhostKind;
  /// The line's number in the file, when it has one.
  no: number | null;
}

/// One key to press: a character of a line, or Enter at its end.
export interface Cell {
  line: number;
  col: number;
  ch: string;
}

export interface Course {
  lines: GhostLine[];
  cells: Cell[];
  /// Per line, the cells it owns: `[first, end)`.
  ranges: Array<[number, number]>;
}

/// Past this the page would hold too many lines to stay quick; the rest is
/// left out and said so.
export const MAX_LINES = 4000;

/// Lines of a file, all to be typed. `from` numbers the first one.
export function fromText(text: string, from = 1): GhostLine[] {
  const lines = text.replace(/\r\n?/g, '\n').split('\n');
  if (lines.length > 1 && lines[lines.length - 1] === '') lines.pop();
  return lines.map((t, i) => ({ text: t, kind: 'type', no: from + i }));
}

/// What a unified diff added, to be typed, with its context lines given.
/// The removed lines are not there to type at all.
export function fromDiff(diff: string[]): GhostLine[] {
  const out: GhostLine[] = [];
  let n = 0;
  let inHunk = false;
  for (const l of diff) {
    const h = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(l);
    if (h) {
      if (out.length) out.push({ text: '', kind: 'gap', no: null });
      n = Number(h[1]);
      inHunk = true;
      continue;
    }
    if (!inHunk) {
      // A new, untracked file comes as `new file` and then only `+` lines.
      if (l.startsWith('+') && !l.startsWith('+++')) out.push({ text: l.slice(1), kind: 'type', no: ++n });
      continue;
    }
    if (l.startsWith('+')) out.push({ text: l.slice(1), kind: 'type', no: n++ });
    else if (l.startsWith(' ')) out.push({ text: l.slice(1), kind: 'given', no: n++ });
    else if (l.startsWith('-') || l.startsWith('\\')) continue;
    else inHunk = false;
  }
  return out;
}

/// Lay the keys out. Indentation is typed only when asked for — otherwise
/// Enter lands at the start of the next line's code, the way an editor
/// would put it. Trailing spaces and blank lines are never typed.
export function course(lines: GhostLine[], typeIndent: boolean): Course {
  const cells: Cell[] = [];
  const ranges: Array<[number, number]> = [];
  const content = (l: GhostLine) => l.kind === 'type' && l.text.trim() !== '';
  for (let i = 0; i < lines.length; i++) {
    const first = cells.length;
    const l = lines[i];
    if (content(l)) {
      const end = l.text.trimEnd().length;
      const start = typeIndent ? 0 : l.text.length - l.text.trimStart().length;
      for (let c = start; c < end; c++) cells.push({ line: i, col: c, ch: l.text[c] });
      // Enter, when the next line with anything on it is also to be typed.
      let j = i + 1;
      while (j < lines.length && lines[j].kind === 'type' && lines[j].text.trim() === '') j++;
      if (j < lines.length && content(lines[j])) cells.push({ line: i, col: end, ch: '\n' });
    }
    ranges.push([first, cells.length]);
  }
  return { lines, cells, ranges };
}

/// Whether a key typed is the one wanted. A non-breaking space in the code
/// is typed as a plain one.
export function matches(want: string, got: string): boolean {
  if (want === got) return true;
  return want === ' ' && got === ' ';
}

/// Words per minute the usual way: five characters to a word.
export function wpm(correct: number, ms: number): number {
  if (ms < 1000) return 0;
  return Math.round(correct / 5 / (ms / 60000));
}

export function accuracy(correct: number, mistakes: number): number {
  const all = correct + mistakes;
  return all === 0 ? 100 : Math.round((correct / all) * 100);
}
