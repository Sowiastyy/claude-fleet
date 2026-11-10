import { useCallback, useEffect, useRef, useState } from 'react';
import { Check, CircleAlert, Cloud, ExternalLink, FileCode2, GitBranch, History, Plus, Settings as SettingsIcon, SquareTerminal, MessageSquare, Sun } from 'lucide-react';
import {
  api,
  errorText,
  on,
  type Account,
  type Accounts,
  type CloudRecent,
  type Event,
  type Recent,
  type GitChange,
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
import { Sidebar, sessionTitle } from './components/Sidebar';
import { AccountMenu } from './components/AccountMenu';
import { Home, type StartOpts } from './components/Home';
import { Conversation } from './components/Conversation';
import { GitPanel } from './components/GitPanel';
import type { Ask, Intent } from './components/Workbench';
import { SettingsDialog } from './components/SettingsDialog';
import { Titlebar } from './components/Titlebar';
import { CommandPalette, type Command } from './components/CommandPalette';
import { isCloudRef } from './commands';

const DEFAULTS: Settings = { background: 'harbor', customImage: null, newChatDim: 65, chatDim: 85, theme: 'harbor', accent: '' };

export function App() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [logs, setLogs] = useState<Record<string, Event[]>>({});
  const [accounts, setAccounts] = useState<Accounts | null>(null);
  const [settings, setSettings] = useState<Settings>(DEFAULTS);
  const [custom, setCustom] = useState<string | null>(null);
  const [git, setGit] = useState<GitState | null>(null);
  // The folder `git` was last read for: until it is the active one, no git
  // state means not read yet rather than not a repository.
  const [gitFor, setGitFor] = useState<string | null>(null);
  const [showGit, setShowGit] = useState(() => localStorage.getItem('fleet.git') === '1');
  const [accountsOpen, setAccountsOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [carry, setCarry] = useState(() => localStorage.getItem('fleet.carry.v2') === '1');
  const [update, setUpdate] = useState<UpdateState | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [recent, setRecent] = useState<Recent[]>([]);
  const [clouds, setClouds] = useState<CloudRecent[]>([]);
  const [toast, setToast] = useState<{ text: string; bad: boolean } | null>(null);
  const toastTimer = useRef<number | undefined>(undefined);
  // Sessions showing the code view instead of their chat or terminal.
  const [code, setCode] = useState<Record<string, boolean>>({});
  const [intent, setIntent] = useState<{ session: string; intent: Intent } | null>(null);
  const intentSeq = useRef(0);

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
    ];
    return () => offs.forEach((p) => p.then((off) => off()));
  }, [notify]);

  useEffect(() => {
    applyTheme(settings.theme, settings.accent);
  }, [settings.theme, settings.accent]);

  // ----- git, for the header and the panel
  const refreshGit = useCallback(() => {
    if (!active) return;
    const cwd = active.cwd;
    api
      .git(cwd)
      .then(setGit)
      .catch(() => setGit(null))
      .finally(() => setGitFor(cwd));
  }, [active?.cwd]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    setGit(null);
    setGitFor(null);
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
    if (o.account) req.account = o.account;
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

  /// A shell in `cwd`: the given folder, else the open session's, else the
  /// one the new-session page last used. It runs as the open session's
  /// account, so the `claude` typed in it does too.
  const openShell = async (cwd?: string) => {
    const dir = cwd ?? active?.cwd ?? localStorage.getItem('fleet.cwd') ?? (await api.home());
    const req: NewSession = { cwd: dir, shell: true };
    if (!cwd && active?.account) req.account = active.account;
    try {
      select(await api.create(req));
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
      if (carry)
        sessions
          .filter((s) => s.view === 'terminal' && !s.shell && s.account !== a.uuid && s.status !== 'working' && s.status !== 'waiting')
          .forEach((s) => termBuffer.reset(s.id));
      notify(await api.switchAccount(a.uuid, carry));
    } catch (e) {
      notify(errorText(e), true);
    }
  };

  const moveSession = async (s: Session, uuid: string) => {
    try {
      if (s.view === 'terminal' && s.status !== 'working') termBuffer.reset(s.id);
      await api.setAccount(s.id, uuid, 30, 100);
      const a = accounts?.accounts.find((x) => x.uuid === uuid);
      notify(`${s.name} now runs as ${a?.email ?? 'that account'}`);
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
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && !e.altKey && e.code === 'Backquote') {
        e.preventDefault();
        e.stopPropagation();
        void openShell();
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
      if (e.key === 'Escape' && (accountsOpen || settingsOpen)) {
        setAccountsOpen(false);
        setSettingsOpen(false);
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [sessions, accountsOpen, settingsOpen, select, active]); // eslint-disable-line react-hooks/exhaustive-deps

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

  /// Show the code view for the active session, and what to open in it.
  const openCode = (ask?: Ask) => {
    if (!active) return;
    setCode((c) => ({ ...c, [active.id]: true }));
    if (ask) setIntent({ session: active.id, intent: { ...ask, seq: ++intentSeq.current } });
  };
  const setCodeFor = (id: string, open: boolean) => setCode((c) => ({ ...c, [id]: open }));

  const openChange = (c: GitChange) =>
    openCode({ kind: 'diff', of: { kind: 'worktree', path: c.path, untracked: c.code === '??' }, label: `${c.path} (uncommitted)` });

  const toggleGit = () => {
    localStorage.setItem('fleet.git', showGit ? '0' : '1');
    setShowGit(!showGit);
  };

  const commands: Command[] = [
    ...sessions.map((s, i) => ({
      id: `s-${s.id}`,
      group: 'Sessions',
      label: sessionTitle(s),
      hint: i < 9 ? `F${i + 1}` : s.cwd,
      icon: <span className={`dot ${s.status}`} style={{ marginTop: 0 }} />,
      run: () => select(s.id),
    })),
    { id: 'new', group: 'Actions', label: 'New session', hint: `F${Math.min(sessions.length + 1, 9)}`, icon: <Plus size={15} />, run: goHome },
    { id: 'shell', group: 'Actions', label: 'New terminal', hint: 'Ctrl+Shift+`', icon: <SquareTerminal size={15} />, run: () => void openShell() },
    ...(active
      ? [
          ...(active.cloud || active.shell
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
          {
            id: 'code',
            group: 'Actions',
            label: code[active.id] ? 'Close the code view' : 'Code view: files, git history, ghost mode',
            icon: <FileCode2 size={15} />,
            run: () => setCodeFor(active.id, !code[active.id]),
          },
          ...(active.cloud && active.cloudUrl
            ? [{ id: 'cloud-open', group: 'Actions', label: 'Open on claude.ai', icon: <ExternalLink size={15} />, run: () => void openUrl(active.cloudUrl) }]
            : []),
        ]
      : []),
    { id: 'settings', group: 'Actions', label: 'Settings', hint: 'Appearance', icon: <SettingsIcon size={15} />, run: () => setSettingsOpen(true) },
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
      <Titlebar title={active ? active.name : 'New session'} version={update?.current} />
      <div className="app-row">
      <Sidebar
        sessions={sessions}
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
        onShell={() => void openShell()}
      />
      <main className="main">
        {bg && <img className="bg-image" src={bg} alt="" aria-hidden />}
        <div className="bg-dim" style={{ opacity: dim }} aria-hidden />
        <div className="main-body">
          {active ? (
            <Conversation
              session={active}
              accounts={accounts}
              onAccount={(uuid) => void moveSession(active, uuid)}
              events={logs[active.id] ?? []}
              git={git}
              showGit={showGit}
              onSend={(t, images) => api.send(active.id, t, images).catch((e) => notify(errorText(e), true))}
              onStop={() => void api.interrupt(active.id)}
              onView={setView}
              onAllow={(rule) => api.allow(active.id, rule).catch((e) => notify(errorText(e), true))}
              onConfigure={(model, mode) => api.configure(active.id, model, mode).catch((e) => notify(errorText(e), true))}
              onToggleGit={toggleGit}
              onError={(t) => notify(t, true)}
              onClear={() => void clearSession()}
              onTerminalCommand={(t) => void runInTerminal(t)}
              onOpenUrl={(u) => void openUrl(u)}
              onTeleport={() => void teleport(active)}
              onRestart={() => {
                termBuffer.reset(active.id);
                api.restartTerm(active.id, 30, 100).catch((e) => notify(errorText(e), true));
              }}
              code={!!code[active.id]}
              gitRead={gitFor === active.cwd}
              onCode={(open) => setCodeFor(active.id, open)}
              codeIntent={intent?.session === active.id ? intent.intent : null}
              onNotify={(t) => notify(t)}
            />
          ) : (
            <Home
              name={firstName}
              accounts={accounts}
              sessions={sessions}
              onStart={start}
              onAttachCloud={(r) => void attachCloud(r)}
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
      {active && showGit && (
        <GitPanel
          cwd={active.cwd}
          git={git}
          onRefresh={refreshGit}
          onToast={notify}
          onOpenCommit={(hash) => openCode({ kind: 'commit', hash })}
          onOpenChange={openChange}
          onClose={toggleGit}
        />
      )}
      </div>
      {accountsOpen && accounts && (
        <AccountMenu
          accounts={accounts}
          sessions={sessions}
          carry={carry}
          onCarry={(c) => {
            localStorage.setItem('fleet.carry.v2', c ? '1' : '0');
            setCarry(c);
          }}
          onChoose={chooseAccount}
          onForget={(a) => api.forgetAccount(a.uuid).catch((e) => notify(errorText(e), true))}
          onAdd={addAccount}
          onSettings={() => {
            setAccountsOpen(false);
            setSettingsOpen(true);
          }}
          onClose={() => setAccountsOpen(false)}
        />
      )}
      {paletteOpen && <CommandPalette commands={commands} onClose={() => setPaletteOpen(false)} />}
      {settingsOpen && (
        <SettingsDialog
          settings={settings}
          custom={custom}
          onChange={saveSettings}
          onCustom={setCustom}
          onClose={() => setSettingsOpen(false)}
          onToast={notify}
        />
      )}
    </div>
  );
}
