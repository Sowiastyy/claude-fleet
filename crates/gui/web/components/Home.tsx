import { useEffect, useRef, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { ArrowUp, Cloud, Folder, FolderOpen, History, Laptop, Palette, SquareTerminal } from 'lucide-react';
import { api, type Attachment, type CloudRecent, type Recent, type Session, type SlashCommand } from '../api';
import { MODELS, MODES, modelChoice, modelName, nextMode } from '../choices';
import { commandsFor, isCloudRef, routeSlash } from '../commands';
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
  /// Run it in the cloud: `prompt` is the task, or a cloud session's id or
  /// link to attach to.
  cloud: boolean;
  /// Typed into the terminal once it is up: a terminal-only command.
  typeIn?: string;
}

const WHERE: Choice[] = [
  { value: 'local', label: 'Local', hint: 'this computer', icon: <Laptop size={14} aria-hidden /> },
  { value: 'cloud', label: 'Cloud', hint: 'claude.ai/code', icon: <Cloud size={14} aria-hidden /> },
];

interface Props {
  name: string;
  sessions: Session[];
  onStart: (opts: StartOpts) => void;
  onResume: (r: Recent) => void;
  onAttachCloud: (r: CloudRecent) => void;
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
  const [model, setModel] = useState(() => modelChoice(stored('fleet.model', '')));
  const [mode, setMode] = useState(() => stored('fleet.mode', 'default'));
  const [recent, setRecent] = useState<Recent[]>([]);
  const [clouds, setClouds] = useState<CloudRecent[]>([]);
  const [where, setWhere] = useState(() => (stored('fleet.where', 'local') === 'cloud' ? 'cloud' : 'local'));
  const [commands, setCommands] = useState<SlashCommand[]>([]);
  const box = useRef<HTMLTextAreaElement>(null);
  const cloud = where === 'cloud';

  useEffect(() => {
    api.projects().then((ps) => {
      setProjects(ps);
      const saved = stored('fleet.cwd', '');
      if (saved) setCwd(saved);
      else if (ps[0]) setCwd(ps[0]);
      else api.home().then(setCwd);
    });
    api.recent().then(setRecent);
    api.cloudRecent().then(setClouds).catch(() => setClouds([]));
    box.current?.focus();
  }, []);

  useEffect(() => {
    if (!cwd) return;
    let live = true;
    void commandsFor(cwd).then((c) => live && setCommands(c));
    return () => {
      live = false;
    };
  }, [cwd]);

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
  const start = (terminal: boolean, typed?: string) => {
    const prompt = (typed ?? draft).trim();
    if (!cwd || (!terminal && !prompt && !images.length)) return;
    const done = () => {
      setDraft('');
      setImages([]);
    };
    if (cloud) {
      if (!prompt) return;
      if (images.length) {
        p.onError('A cloud session takes the task as text only');
        return;
      }
      p.onStart({ cwd, prompt, images: [], model, mode, terminal: true, cloud: true });
      done();
      return;
    }
    const route = images.length || terminal ? { kind: 'chat' as const } : routeSlash(prompt, commands);
    switch (route.kind) {
      case 'clear':
        done();
        return;
      case 'model':
        setModel(modelChoice(route.model));
        store('fleet.model', modelChoice(route.model));
        done();
        return;
      case 'terminal':
        p.onStart({ cwd, prompt: '', images: [], model, mode, terminal: true, cloud: false, typeIn: prompt });
        done();
        return;
    }
    p.onStart({ cwd, prompt, images, model, mode, terminal, cloud: false });
    done();
  };
  const chooseWhere = (v: string) => {
    setWhere(v === 'cloud' ? 'cloud' : 'local');
    store('fleet.where', v);
  };
  const attaching = cloud && isCloudRef(draft);

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
          placeholder={cloud ? 'Describe a task for a cloud session, or paste a claude.ai/code link' : 'Describe a task, or type / for commands'}
          rows={3}
          value={draft}
          onChange={setDraft}
          images={images}
          onImages={setImages}
          onError={p.onError}
          cwd={cwd}
          commands={cloud ? undefined : commands}
          onSubmit={(t) => start(false, t)}
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
          {cloud ? (
            <span className="small mist cloud-hint">
              <Cloud size={13} aria-hidden />
              {attaching ? 'Attach to this cloud session' : 'Runs on claude.ai, in its terminal here'}
            </span>
          ) : (
            <button type="button" className="icon-btn" title="Open in the terminal" aria-label="Open in the terminal" onClick={() => start(true)}>
              <SquareTerminal size={17} aria-hidden />
            </button>
          )}
          <div style={{ flex: 1 }} />
          <button type="button" className="send-btn" aria-label={cloud ? 'Start in the cloud' : 'Start'} disabled={!ready || !cwd} onClick={() => start(false)}>
            <ArrowUp size={17} strokeWidth={2.25} aria-hidden />
          </button>
        </Composer>

        <div className="pills">
          <Picker
            label="Where"
            variant="pill"
            placement="down"
            value={where}
            icon={cloud ? <Cloud size={14} aria-hidden /> : <Laptop size={14} aria-hidden />}
            choices={WHERE}
            onChange={chooseWhere}
          />
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
            display={modelName(model)}
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

        {clouds.length > 0 && (
          <section className="recent" aria-label="Cloud sessions">
            <h2>Cloud sessions</h2>
            {clouds.slice(0, 4).map((r) => (
              <button key={r.id} type="button" className="recent-row" onClick={() => p.onAttachCloud(r)} title={r.url}>
                <Cloud size={15} color="var(--mist)" aria-hidden />
                <span className="ellipsis" style={{ flex: 1, minWidth: 0 }}>{r.task || r.id}</span>
                <span className="small mist">{folderName(r.cwd)}</span>
                <span className="small faint" style={{ width: 34, textAlign: 'right' }}>{fmtAgo(Math.max(0, (Date.now() - r.at) / 1000))}</span>
              </button>
            ))}
          </section>
        )}

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
