import { useCallback, useEffect, useRef, useState } from 'react';
import { Check, CircleAlert } from 'lucide-react';
import {
  api,
  errorText,
  on,
  type Account,
  type Accounts,
  type Event,
  type Foreign,
  type GitState,
  type Session,
  type Settings,
  type View,
} from './api';
import { backgroundUrl } from './backgrounds';
import { applyTheme } from './themes';
import { termBuffer } from './termstore';
import { Sidebar } from './components/Sidebar';
import { AccountMenu } from './components/AccountMenu';
import { Home, type StartOpts } from './components/Home';
import { Conversation } from './components/Conversation';
import { GitPanel } from './components/GitPanel';
import { AppearanceDialog } from './components/AppearanceDialog';

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
    const offs = [
      on<Session[]>('fleet://sessions', setSessions),
      on<{ id: string; event: Event }>('fleet://event', ({ id, event }) =>
        setLogs((l) => ({ ...l, [id]: [...(l[id] ?? []), event] })),
      ),
      on<{ id: string; data: string }>('fleet://term', ({ id, data }) => termBuffer.push(id, data)),
      on<{ id: string }>('fleet://term-reset', ({ id }) => termBuffer.reset(id)),
      on<Accounts>('fleet://accounts', setAccounts),
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
    try {
      const id = await api.create({
        cwd: o.cwd,
        prompt: o.terminal ? undefined : o.prompt,
        images: o.terminal ? [] : o.images,
        model: o.model || undefined,
        permissionMode: o.mode,
        terminal: o.terminal,
      });
      select(id);
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

  const saveSettings = (s: Settings) => {
    setSettings(s);
    void api.saveSettings(s);
  };

  // ----- keys: F1-F9 jump to a session, even from inside the terminal
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
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
              onToggleGit={() => {
                localStorage.setItem('fleet.git', showGit ? '0' : '1');
                setShowGit(!showGit);
              }}
              onAppearance={() => setBgOpen(true)}
              onError={(t) => notify(t, true)}
            />
          ) : (
            <Home
              name={firstName}
              sessions={sessions}
              onStart={start}
              onResume={async (r) => {
                try {
                  select(await api.create({ cwd: r.cwd, resume: r.id }));
                } catch (e) {
                  notify(errorText(e), true);
                }
              }}
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
