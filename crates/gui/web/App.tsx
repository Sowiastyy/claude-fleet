import { useCallback, useEffect, useRef, useState } from 'react';
import { Check, CircleAlert, Cloud, ExternalLink, GitBranch, History, Palette, Plus, SquareTerminal, MessageSquare, Sun } from 'lucide-react';
import {
  api,
  errorText,
  on,
  type Account,
  type Accounts,
  type CloudRecent,
  type Event,
  type Foreign,
  type Recent,
  type GitState,
  type NewSession,
  type Session,
  type Settings,
  type UpdateState,
  type View,
} from './api';
import { backgroundUrl } from './backgrounds';
import { THEMES, applyTheme } from './themes';
import { termBuffer } from './termstore';
import { Sidebar } from './components/Sidebar';
import { AccountMenu } from './components/AccountMenu';
import { Home, type StartOpts } from './components/Home';
import { Conversation } from './components/Conversation';
import { GitPanel } from './components/GitPanel';
import { AppearanceDialog } from './components/AppearanceDialog';
import { CommandPalette, type Command } from './components/CommandPalette';
import { isCloudRef } from './commands';

const DEFAULTS: Settings = { background: 'harbor', customImage: null, newChatDim: 65, chatDim: 85, theme: 'harbor', accent: '' };

export function App() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [logs, setLogs] = useState<Record<string, Event[]>>({});
  const [accounts, setAccounts] = useState<Accounts | null>(null);
  const [foreign, setForeign] = useState<Foreign[]>([]);
  const [settings, setSettings] = useState<Settings>(DEFAULTS);
  const [custom, setCustom] = useState<string | null>(null);
  const [git, setGit] = useState<GitState | null>(null);
  const [showGit, setShowGit] = useState(() => localStorage.getItem('fleet.git') === '1');
  const [accountsOpen, setAccountsOpen] = useState(false);
  const [bgOpen, setBgOpen] = useState(false);
  const [carry, setCarry] = useState(() => localStorage.getItem('fleet.carry') !== '0');
  const [update, setUpdate] = useState<UpdateState | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [recent, setRecent] = useState<Recent[]>([]);
  const [clouds, setClouds] = useState<CloudRecent[]>([]);
  const [toast, setToast] = useState<{ text: string; bad: boolean } | null>(null);
  const toastTimer = useRef<number | undefined>(undefined);
  const ticks = useRef(0);

  const active = sessions.find((s) => s.id === activeId) ?? null;

  const notify = useCallback((text: string, bad = false) => {
    setToast({ text, bad });
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), bad ? 6000 : 3500);
  }, []);

  const loadLog = useCallback(async (id: string) => {
    const log = await api.log(id);
    setLogs((l) => ({ ...l, [id]: log }));
  }, []);

  // ----- startup and events
  useEffect(() => {
    api.sessions().then(setSessions);
    api.accounts().then(setAccounts);
    api.foreign().then(setForeign);
    api.settings().then((s) => setSettings({ ...DEFAULTS, ...s }));
    api.customBackground().then(setCustom);
    api.updateState().then(setUpdate);
    const offs = [
      on<Session[]>('fleet://sessions', setSessions),
      on<{ id: string; event: Event }>('fleet://event', ({ id, event }) =>
        setLogs((l) => ({ ...l, [id]: [...(l[id] ?? []), event] })),
      ),
      on<{ id: string; data: string }>('fleet://term', ({ id, data }) => termBuffer.push(id, data)),
      on<{ id: string }>('fleet://term-reset', ({ id }) => termBuffer.reset(id)),
      on<Accounts>('fleet://accounts', setAccounts),
      on<UpdateState>('fleet://update', setUpdate),
      on<string>('fleet://toast', (t) => notify(t)),
      on<unknown>('fleet://tick', () => {
        ticks.current += 1;
        if (ticks.current % 3 === 0) api.foreign().then(setForeign);
      }),
    ];
    return () => offs.forEach((p) => p.then((off) => off()));
  }, [notify]);

  useEffect(() => {
    applyTheme(settings.theme, settings.accent);
  }, [settings.theme, settings.accent]);

  // ----- git, for the header and the panel
  const refreshGit = useCallback(() => {
    if (!active) return;
    api.git(active.cwd).then(setGit).catch(() => setGit(null));
  }, [active?.cwd]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    setGit(null);
    if (!active) return;
    refreshGit();
    const t = window.setInterval(refreshGit, 4000);
    return () => window.clearInterval(t);
  }, [refreshGit]); // eslint-disable-line react-hooks/exhaustive-deps

  // ----- actions
  const select = useCallback(
    (id: string) => {
      setActiveId(id);
      setAccountsOpen(false);
      void loadLog(id);
    },
    [loadLog],
  );

  const goHome = () => {
    setActiveId(null);
    setAccountsOpen(false);
  };

  const start = async (o: StartOpts) => {
    const req: NewSession = o.cloud
      ? isCloudRef(o.prompt)
        ? { cwd: o.cwd, cloud: true, resume: o.prompt.trim() }
        : { cwd: o.cwd, cloud: true, prompt: o.prompt, model: o.model || undefined, permissionMode: o.mode }
      : {
          cwd: o.cwd,
          prompt: o.terminal ? undefined : o.prompt,
          images: o.terminal ? [] : o.images,
          model: o.model || undefined,
          permissionMode: o.mode,
          terminal: o.terminal,
          typeIn: o.typeIn,
        };
    try {
      select(await api.create(req));
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const attachCloud = async (r: CloudRecent) => {
    try {
      select(await api.create({ cwd: r.cwd, cloud: true, resume: r.id }));
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const teleport = async (s: Session) => {
    try {
      select(await api.create({ cwd: s.cwd, teleport: s.cloudId }));
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const openUrl = (url: string) => api.openUrl(url).catch((e) => notify(errorText(e), true));

  const runInTerminal = async (text: string) => {
    if (!active) return;
    if (active.view === 'chat') termBuffer.reset(active.id);
    try {
      await api.terminalCommand(active.id, text, 30, 100);
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const clearSession = async () => {
    if (!active) return;
    try {
      await api.clear(active.id);
      await loadLog(active.id);
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const setView = async (v: View) => {
    if (!active || active.view === v) return;
    if (v === 'terminal') termBuffer.reset(active.id);
    try {
      await api.setView(active.id, v, 30, 100);
      if (v === 'chat') await loadLog(active.id);
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const close = async (id: string) => {
    await api.close(id);
    termBuffer.reset(id);
    if (id === activeId) setActiveId(null);
  };

  const chooseAccount = async (a: Account) => {
    setAccountsOpen(false);
    if (a.current) return;
    try {
      if (carry) sessions.filter((s) => s.view === 'terminal').forEach((s) => termBuffer.reset(s.id));
      notify(await api.switchAccount(a.uuid, carry));
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const addAccount = async () => {
    setAccountsOpen(false);
    try {
      const id = await api.addAccount(active?.cwd ?? (await api.home()));
      select(id);
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const installUpdate = async () => {
    try {
      setUpdate(await api.updateInstall());
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const restart = () => api.restart().catch((e) => notify(errorText(e), true));

  const saveSettings = (s: Settings) => {
    setSettings(s);
    void api.saveSettings(s);
  };

  // ----- keys: F1-F9 jump to a session, even from inside the terminal
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        e.stopPropagation();
        setPaletteOpen((o) => !o);
        return;
      }
      const m = /^F([1-9])$/.exec(e.key);
      if (m) {
        const i = Number(m[1]) - 1;
        e.preventDefault();
        e.stopPropagation();
        if (i < sessions.length) select(sessions[i].id);
        else if (i === sessions.length) goHome();
        return;
      }
      if (e.key === 'Escape' && (accountsOpen || bgOpen)) {
        setAccountsOpen(false);
        setBgOpen(false);
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [sessions, accountsOpen, bgOpen, select]);

  useEffect(() => {
    if (!paletteOpen) return;
    api.recent().then(setRecent).catch(() => setRecent([]));
    api.cloudRecent().then(setClouds).catch(() => setClouds([]));
  }, [paletteOpen]);

  const resume = async (r: Recent) => {
    try {
      select(await api.create({ cwd: r.cwd, resume: r.id }));
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const toggleGit = () => {
    localStorage.setItem('fleet.git', showGit ? '0' : '1');
    setShowGit(!showGit);
  };

  const commands: Command[] = [
    ...sessions.map((s, i) => ({
      id: `s-${s.id}`,
      group: 'Sessions',
      label: s.name,
      hint: i < 9 ? `F${i + 1}` : s.cwd,
      icon: <span className={`dot ${s.status}`} style={{ marginTop: 0 }} />,
      run: () => select(s.id),
    })),
    { id: 'new', group: 'Actions', label: 'New session', hint: `F${Math.min(sessions.length + 1, 9)}`, icon: <Plus size={15} />, run: goHome },
    ...(active
      ? [
          ...(active.cloud
            ? []
            : [
                {
                  id: 'view',
                  group: 'Actions',
                  label: active.view === 'chat' ? 'Show terminal' : 'Show chat',
                  icon: active.view === 'chat' ? <SquareTerminal size={15} /> : <MessageSquare size={15} />,
                  run: () => void setView(active.view === 'chat' ? 'terminal' : 'chat'),
                },
              ]),
          { id: 'git', group: 'Actions', label: showGit ? 'Hide git panel' : 'Show git panel', icon: <GitBranch size={15} />, run: toggleGit },
          ...(active.cloud && active.cloudUrl
            ? [{ id: 'cloud-open', group: 'Actions', label: 'Open on claude.ai', icon: <ExternalLink size={15} />, run: () => void openUrl(active.cloudUrl) }]
            : []),
        ]
      : []),
    { id: 'appearance', group: 'Actions', label: 'Appearance', icon: <Palette size={15} />, run: () => setBgOpen(true) },
    ...THEMES.map((t) => ({
      id: `theme-${t.key}`,
      group: 'Theme',
      label: `Theme: ${t.label}`,
      hint: settings.theme === t.key ? 'current' : undefined,
      icon: <Sun size={15} />,
      run: () => saveSettings({ ...settings, theme: t.key }),
    })),
    ...clouds.map((r) => ({
      id: `c-${r.id}`,
      group: 'Cloud sessions',
      label: r.task || r.id,
      hint: r.id,
      icon: <Cloud size={15} />,
      run: () => void attachCloud(r),
    })),
    ...recent.map((r) => ({
      id: `r-${r.id}`,
      group: 'Recent conversations',
      label: r.summary,
      hint: r.project,
      icon: <History size={15} />,
      run: () => void resume(r),
    })),
  ];

  const me = accounts?.accounts.find((a) => a.current);
  const firstName = (me?.name ?? '').split(/\s+/)[0] ?? '';
  const bg = backgroundUrl(settings.background, custom);
  const dim = (active ? settings.chatDim : settings.newChatDim) / 100;

  return (
    <div className="app">
      <Sidebar
        sessions={sessions}
        foreign={foreign}
        activeId={activeId}
        accounts={accounts}
        accountsOpen={accountsOpen}
        onHome={goHome}
        onSelect={select}
        onClose={close}
        onAccounts={() => setAccountsOpen(!accountsOpen)}
        update={update}
        onUpdate={installUpdate}
        onRestart={restart}
        onPalette={() => setPaletteOpen(true)}
      />
      <main className="main">
        {bg && <img className="bg-image" src={bg} alt="" aria-hidden />}
        <div className="bg-dim" style={{ opacity: dim }} aria-hidden />
        <div className="main-body">
          {active ? (
            <Conversation
              session={active}
              events={logs[active.id] ?? []}
              git={git}
              showGit={showGit}
              onSend={(t, images) => api.send(active.id, t, images).catch((e) => notify(errorText(e), true))}
              onStop={() => void api.interrupt(active.id)}
              onView={setView}
              onAllow={(rule) => api.allow(active.id, rule).catch((e) => notify(errorText(e), true))}
              onConfigure={(model, mode) => api.configure(active.id, model, mode).catch((e) => notify(errorText(e), true))}
              onToggleGit={toggleGit}
              onAppearance={() => setBgOpen(true)}
              onError={(t) => notify(t, true)}
              onClear={() => void clearSession()}
              onTerminalCommand={(t) => void runInTerminal(t)}
              onOpenUrl={(u) => void openUrl(u)}
              onTeleport={() => void teleport(active)}
              onRestart={() => {
                termBuffer.reset(active.id);
                api.restartTerm(active.id, 30, 100).catch((e) => notify(errorText(e), true));
              }}
            />
          ) : (
            <Home
              name={firstName}
              sessions={sessions}
              onStart={start}
              onResume={resume}
              onAttachCloud={(r) => void attachCloud(r)}
              onAppearance={() => setBgOpen(true)}
              onError={(t) => notify(t, true)}
            />
          )}
        </div>
        {toast && (
          <div className={`toast ${toast.bad ? 'bad' : ''}`} role="status">
            {toast.bad ? <CircleAlert size={16} color="var(--danger)" aria-hidden /> : <Check size={16} color="var(--idle)" aria-hidden />}
            <span className="ellipsis">{toast.text}</span>
          </div>
        )}
      </main>
      {active && showGit && <GitPanel cwd={active.cwd} git={git} onRefresh={refreshGit} onToast={notify} />}
      {accountsOpen && accounts && (
        <AccountMenu
          accounts={accounts}
          carry={carry}
          onCarry={(c) => {
            localStorage.setItem('fleet.carry', c ? '1' : '0');
            setCarry(c);
          }}
          onChoose={chooseAccount}
          onForget={(a) => api.forgetAccount(a.uuid).catch((e) => notify(errorText(e), true))}
          onAdd={addAccount}
          onClose={() => setAccountsOpen(false)}
        />
      )}
      {paletteOpen && <CommandPalette commands={commands} onClose={() => setPaletteOpen(false)} />}
      {bgOpen && (
        <AppearanceDialog
          settings={settings}
          custom={custom}
          onChange={saveSettings}
          onCustom={setCustom}
          onClose={() => setBgOpen(false)}
          onToast={notify}
        />
      )}
    </div>
  );
}
