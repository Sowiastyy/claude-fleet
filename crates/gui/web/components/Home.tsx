import { useEffect, useRef, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { ArrowUp, ChevronDown, Folder, History, ImageIcon, ShieldAlert, SquareTerminal } from 'lucide-react';
import { api, type Recent, type Session } from '../api';
import { fmtAgo } from '../messages';

export const MODELS: Array<[string, string]> = [
  ['', 'Default model'],
  ['opus', 'Opus'],
  ['sonnet', 'Sonnet'],
  ['haiku', 'Haiku'],
];

export const MODES: Array<[string, string]> = [
  ['default', 'Asks first'],
  ['acceptEdits', 'Accepts edits'],
  ['plan', 'Plans only'],
  ['bypassPermissions', 'Asks nothing'],
];

const CHOOSE = '\u0000choose';

function folderName(path: string): string {
  const parts = path.replace(/[\\/]+$/, '').split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function Pill(p: {
  label: string;
  icon?: React.ReactNode;
  value: string;
  options: Array<[string, string]>;
  onChange: (v: string) => void;
}) {
  return (
    <label className="pill">
      {p.icon}
      <span className="sr-only">{p.label}</span>
      <select value={p.value} onChange={(e) => p.onChange(e.target.value)}>
        {p.options.map(([v, l]) => (
          <option key={v} value={v}>
            {l}
          </option>
        ))}
      </select>
      <ChevronDown className="chev" size={13} aria-hidden />
    </label>
  );
}

function greeting(): string {
  const h = new Date().getHours();
  return h < 5 ? 'Late night' : h < 12 ? 'Morning' : h < 18 ? 'Afternoon' : 'Evening';
}

interface Props {
  name: string;
  sessions: Session[];
  onStart: (opts: { cwd: string; prompt: string; model: string; mode: string; terminal: boolean }) => void;
  onResume: (r: Recent) => void;
  onBackground: () => void;
}

export function Home(p: Props) {
  const [draft, setDraft] = useState('');
  const [cwd, setCwd] = useState('');
  const [projects, setProjects] = useState<string[]>([]);
  const [model, setModel] = useState(() => localStorage.getItem('fleet.model') ?? '');
  const [mode, setMode] = useState(() => localStorage.getItem('fleet.mode') ?? 'default');
  const [recent, setRecent] = useState<Recent[]>([]);
  const box = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    api.projects().then((ps) => {
      setProjects(ps);
      const saved = localStorage.getItem('fleet.cwd');
      if (saved) setCwd(saved);
      else if (ps[0]) setCwd(ps[0]);
      else api.home().then(setCwd);
    });
    api.recent().then(setRecent);
    box.current?.focus();
  }, []);

  const pickFolder = async () => {
    const dir = await open({ directory: true, multiple: false, defaultPath: cwd || undefined });
    if (typeof dir === 'string') choose(dir);
  };
  const choose = (dir: string) => {
    setCwd(dir);
    localStorage.setItem('fleet.cwd', dir);
  };
  const start = (terminal: boolean) => {
    if (!cwd || (!terminal && !draft.trim())) return;
    p.onStart({ cwd, prompt: draft.trim(), model, mode, terminal });
    setDraft('');
  };

  const waiting = p.sessions.filter((s) => s.status === 'waiting');
  const working = p.sessions.filter((s) => s.status === 'working').length;
  const summary = !p.sessions.length
    ? ''
    : waiting.length
      ? `${p.sessions.length} running, ${waiting.map((s) => s.name).join(' and ')} ${waiting.length === 1 ? 'needs' : 'need'} you`
      : `${p.sessions.length} running, ${working} working`;

  const folderOptions: Array<[string, string]> = [
    ...(cwd && !projects.includes(cwd) ? [[cwd, folderName(cwd)] as [string, string]] : []),
    ...projects.map((d) => [d, folderName(d)] as [string, string]),
    [CHOOSE, 'Choose folder…'],
  ];

  return (
    <>
      <div className="topbar-home">
        <button type="button" className="ghost-btn" onClick={p.onBackground}>
          <ImageIcon size={15} aria-hidden />
          Background
        </button>
      </div>
      <div className="home">
        <h1 className="greeting">
          {greeting()}
          {p.name ? `, ${p.name}` : ''}
        </h1>
        {summary && <p className="summary">{summary}</p>}

        <div className="composer">
          <label htmlFor="composer-home" className="sr-only">Task</label>
          <textarea
            id="composer-home"
            ref={box}
            rows={3}
            placeholder="Describe a task"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                start(false);
              }
            }}
          />
          <div className="composer-row">
            <button type="button" className="icon-btn" title="Open in the terminal" aria-label="Open in the terminal" onClick={() => start(true)}>
              <SquareTerminal size={17} aria-hidden />
            </button>
            <div style={{ flex: 1 }} />
            <button type="button" className="send-btn" aria-label="Start" disabled={!draft.trim() || !cwd} onClick={() => start(false)}>
              <ArrowUp size={17} strokeWidth={2.25} aria-hidden />
            </button>
          </div>
        </div>

        <div className="pills">
          <Pill
            label="Folder"
            icon={<Folder size={14} aria-hidden />}
            value={cwd}
            options={folderOptions}
            onChange={(v) => (v === CHOOSE ? void pickFolder() : choose(v))}
          />
          <Pill
            label="Model"
            value={model}
            options={MODELS}
            onChange={(v) => {
              setModel(v);
              localStorage.setItem('fleet.model', v);
            }}
          />
          <Pill
            label="Permissions"
            icon={<ShieldAlert size={14} aria-hidden />}
            value={mode}
            options={MODES}
            onChange={(v) => {
              setMode(v);
              localStorage.setItem('fleet.mode', v);
            }}
          />
        </div>

        {recent.length > 0 && (
          <section className="recent" aria-label="Recent conversations">
            <h2>Recent</h2>
            {recent.slice(0, 6).map((r) => (
              <button key={r.id} type="button" className="recent-row" onClick={() => p.onResume(r)}>
                <History size={15} color="var(--mist)" aria-hidden />
                <span className="ellipsis" style={{ flex: 1, minWidth: 0 }}>{r.summary}</span>
                <span className="small mist">{r.project}</span>
                <span className="small faint" style={{ width: 34, textAlign: 'right' }}>{fmtAgo(r.agoSecs)}</span>
              </button>
            ))}
          </section>
        )}
      </div>
    </>
  );
}
