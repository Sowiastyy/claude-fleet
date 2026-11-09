import { useEffect, useRef, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { ArrowUp, Folder, FolderOpen, History, Palette, SquareTerminal } from 'lucide-react';
import { api, type Attachment, type Recent, type Session } from '../api';
import { MODELS, MODES, nextMode } from '../choices';
import { fmtAgo } from '../messages';
import { Composer } from './Composer';
import { Picker, type Choice } from './Picker';

const CHOOSE = '\u0000choose';

function folderName(path: string): string {
  const parts = path.replace(/[\\/]+$/, '').split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function greeting(): string {
  const h = new Date().getHours();
  return h < 5 ? 'Late night' : h < 12 ? 'Morning' : h < 18 ? 'Afternoon' : 'Evening';
}

export interface StartOpts {
  cwd: string;
  prompt: string;
  images: Attachment[];
  model: string;
  mode: string;
  terminal: boolean;
}

interface Props {
  name: string;
  sessions: Session[];
  onStart: (opts: StartOpts) => void;
  onResume: (r: Recent) => void;
  onAppearance: () => void;
  onError: (text: string) => void;
}

function stored(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Remembering is a nicety.
  }
}

export function Home(p: Props) {
  const [draft, setDraft] = useState('');
  const [images, setImages] = useState<Attachment[]>([]);
  const [cwd, setCwd] = useState('');
  const [projects, setProjects] = useState<string[]>([]);
  const [model, setModel] = useState(() => stored('fleet.model', ''));
  const [mode, setMode] = useState(() => stored('fleet.mode', 'default'));
  const [recent, setRecent] = useState<Recent[]>([]);
  const box = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    api.projects().then((ps) => {
      setProjects(ps);
      const saved = stored('fleet.cwd', '');
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
    store('fleet.cwd', dir);
  };
  const chooseMode = (v: string) => {
    setMode(v);
    store('fleet.mode', v);
  };
  const ready = !!draft.trim() || images.length > 0;
  const start = (terminal: boolean) => {
    if (!cwd || (!terminal && !ready)) return;
    p.onStart({ cwd, prompt: draft.trim(), images, model, mode, terminal });
    setDraft('');
    setImages([]);
  };

  const waiting = p.sessions.filter((s) => s.status === 'waiting');
  const working = p.sessions.filter((s) => s.status === 'working').length;
  const summary = !p.sessions.length
    ? ''
    : waiting.length
      ? `${p.sessions.length} running, ${waiting.map((s) => s.name).join(' and ')} ${waiting.length === 1 ? 'needs' : 'need'} you`
      : `${p.sessions.length} running, ${working} working`;

  const folders: Choice[] = [
    ...(cwd && !projects.includes(cwd) ? [{ value: cwd, label: folderName(cwd), hint: cwd, icon: <Folder size={14} aria-hidden /> }] : []),
    ...projects.map((d) => ({ value: d, label: folderName(d), hint: d, icon: <Folder size={14} aria-hidden /> })),
    { value: CHOOSE, label: 'Choose folder…', icon: <FolderOpen size={14} aria-hidden />, divide: true },
  ];

  return (
    <>
      <div className="topbar-home">
        <button type="button" className="ghost-btn" onClick={p.onAppearance}>
          <Palette size={15} aria-hidden />
          Appearance
        </button>
      </div>
      <div className="home">
        <h1 className="greeting">
          {greeting()}
          {p.name ? `, ${p.name}` : ''}
        </h1>
        {summary && <p className="summary">{summary}</p>}

        <Composer
          id="composer-home"
          ref={box}
          label="Task"
          placeholder="Describe a task"
          rows={3}
          value={draft}
          onChange={setDraft}
          images={images}
          onImages={setImages}
          onError={p.onError}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              start(false);
            } else if (e.key === 'Tab' && e.shiftKey) {
              e.preventDefault();
              chooseMode(nextMode(mode));
            }
          }}
        >
          <button type="button" className="icon-btn" title="Open in the terminal" aria-label="Open in the terminal" onClick={() => start(true)}>
            <SquareTerminal size={17} aria-hidden />
          </button>
          <div style={{ flex: 1 }} />
          <button type="button" className="send-btn" aria-label="Start" disabled={!ready || !cwd} onClick={() => start(false)}>
            <ArrowUp size={17} strokeWidth={2.25} aria-hidden />
          </button>
        </Composer>

        <div className="pills">
          <Picker
            label="Folder"
            variant="pill"
            placement="down"
            value={cwd}
            display={cwd ? folderName(cwd) : 'Folder'}
            icon={<Folder size={14} aria-hidden />}
            choices={folders}
            onChange={(v) => (v === CHOOSE ? void pickFolder() : choose(v))}
          />
          <Picker
            label="Model"
            variant="pill"
            placement="down"
            value={model}
            choices={MODELS}
            onChange={(v) => {
              setModel(v);
              store('fleet.model', v);
            }}
          />
          <Picker
            label="Permissions"
            variant="pill"
            placement="down"
            value={mode}
            choices={MODES}
            onChange={chooseMode}
            foot={<><kbd>Shift</kbd>+<kbd>Tab</kbd></>}
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
