import { useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import ReactMarkdown, { type Components } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import {
  ArrowDown,
  ArrowUp,
  Bot,
  Brain,
  Check,
  ChevronRight,
  Circle,
  CircleAlert,
  CircleDot,
  Cloud,
  Copy,
  ExternalLink,
  RefreshCw,
  RotateCw,
  FilePlus,
  FileCode2,
  FileText,
  GitBranch,
  GitFork,
  Globe,
  ListTodo,
  LoaderCircle,
  MessageSquare,
  MonitorDown,
  Palette,
  Pencil,
  Sailboat,
  Search,
  ShieldAlert,
  Square,
  SquareCheck,
  SquareTerminal,
  Undo2,
  X,
} from 'lucide-react';
import type { Accounts, Attachment, Event, GitState, Session, SlashCommand, View } from '../api';
import { MODELS, MODES, modelChoice, modelName, nextMode } from '../choices';
import { commandsFor, routeSlash } from '../commands';
import { copyText } from '../clipboard';
import {
  allowRule,
  editDiff,
  editedFiles,
  fmtElapsed,
  latestTodos,
  relative,
  runningTool,
  stepsSummary,
  toBlocks,
  toItems,
  toolSubject,
  turnStart,
  type Block,
  type Denial,
  type Item,
  type ThinkingItem,
  type Todo,
  type ToolItem,
} from '../messages';
import { Composer } from './Composer';
import { Picker } from './Picker';
import { accountChoices } from './AccountPicker';
import { accountColor, accountInitial, accountOf, accountShort } from '../accounts';
import { statusLabel } from './Sidebar';
import { TerminalView } from './TerminalView';
import { CloudPreview } from './CloudPreview';
import { Workbench, type Intent } from './Workbench';

interface Props {
  session: Session;
  accounts: Accounts | null;
  /// Run the session as another account.
  onAccount: (uuid: string) => void;
  events: Event[];
  git: GitState | null;
  showGit: boolean;
  onSend: (text: string, images: Attachment[]) => void;
  onStop: () => void;
  onView: (v: View) => void;
  onAllow: (rule: string) => void;
  onConfigure: (model: string | null, mode: string | null) => void;
  onToggleGit: () => void;
  onAppearance: () => void;
  onError: (text: string) => void;
  /// `/clear`: start over on a new conversation.
  onClear: () => void;
  /// Run a command only the interactive terminal has.
  onTerminalCommand: (text: string) => void;
  onOpenUrl: (url: string) => void;
  /// Pull this cloud session into a local session.
  onTeleport: () => void;
  /// Start the terminal again after it exited.
  onRestart: () => void;
  /// The code view (files, history, ghost mode) shows instead of the session.
  code: boolean;
  /// `git` has been read for this session's folder.
  gitRead: boolean;
  onCode: (open: boolean) => void;
  codeIntent: Intent | null;
  onNotify: (text: string) => void;
}

// Cloud sessions shown as their terminal rather than their claude.ai page.
const asTerminal = new Set<string>();

export function Conversation(p: Props) {
  const s = p.session;
  const branch = p.git?.branch;
  const [, redraw] = useState(0);
  const [back, setBack] = useState(0);
  const previewing = s.cloud && !!s.cloudUrl && !asTerminal.has(s.id);
  const preview = (on: boolean) => {
    if (on) asTerminal.delete(s.id);
    else asTerminal.add(s.id);
    redraw((n) => n + 1);
    if (!on && s.view !== 'terminal') p.onView('terminal');
  };
  return (
    <>
      <header className="header">
        <span className={`dot ${s.status}`} aria-hidden />
        <div className="stack" style={{ flex: 'none', maxWidth: '40%' }}>
          <span className="ellipsis" style={{ fontWeight: 600, fontSize: 15 }}>{s.name}</span>
          <span className="small mist ellipsis" title={s.cloud ? s.cloudTask : undefined}>
            {s.cloud && s.cloudTask ? s.cloudTask : s.cwd}
            {!s.cloud && branch ? ` on ${branch}` : ''}
          </span>
        </div>
        <span className={`chip status-text ${s.status}`}>{statusLabel(s)}</span>
        {(p.accounts?.accounts.length ?? 0) > 1 && (
          <div className="header-acct" title={s.status === 'working' ? 'Can move once the turn is over' : 'Run this session as another account'}>
            <Picker
              label="Account"
              variant="pill"
              placement="down"
              value={s.account}
              display={s.account ? accountShort(accountOf(p.accounts, s.account)) : 'Signed-in account'}
              icon={
                <span className="acct-chip sm" style={{ '--acct': accountColor(p.accounts, s.account) } as CSSProperties}>
                  {accountInitial(accountOf(p.accounts, s.account))}
                </span>
              }
              choices={accountChoices(p.accounts)}
              onChange={p.onAccount}
            />
          </div>
        )}
        {s.cloud && (
          <span className="chip cloud-chip" title={s.cloudId || 'Waiting for the cloud session link'}>
            <Cloud size={12} aria-hidden /> {s.cloudId ? 'cloud' : 'cloud, starting'}
          </span>
        )}
        <div style={{ flex: 1 }} />
        {s.view === 'terminal' && !s.alive && s.status !== 'starting' && (
          <button type="button" className="btn btn-icon-text" onClick={p.onRestart} title="Its process has exited">
            <RotateCw size={13} aria-hidden /> Restart
          </button>
        )}
        {previewing && !p.code && (
          <button
            type="button"
            className="icon-btn boxed"
            aria-label="Back to the session"
            title="Back to this session's page"
            onClick={() => setBack((n) => n + 1)}
          >
            <RefreshCw size={15} aria-hidden />
          </button>
        )}
        {s.cloud && s.cloudUrl && (
          <button type="button" className="btn btn-icon-text" onClick={() => p.onOpenUrl(s.cloudUrl)} title={s.cloudUrl}>
            <ExternalLink size={13} aria-hidden /> claude.ai
          </button>
        )}
        {s.cloud && s.cloudId && (
          <button type="button" className="btn btn-icon-text" onClick={p.onTeleport} title="Continue this cloud session in a local session (claude --teleport)">
            <MonitorDown size={13} aria-hidden /> Teleport
          </button>
        )}
        {s.view === 'chat' && s.status === 'working' && (
          <button type="button" className="btn" onClick={p.onStop} style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <Square size={13} aria-hidden /> Stop
          </button>
        )}
        {s.cloud ? (
          <div className="segmented" role="group" aria-label="View">
            <button
              type="button"
              aria-pressed={!p.code && previewing}
              disabled={!s.cloudUrl}
              title={s.cloudUrl ? "The session's claude.ai page, here" : 'Waiting for the cloud session link'}
              onClick={() => {
                p.onCode(false);
                preview(true);
              }}
            >
              <Cloud size={14} aria-hidden /> Preview
            </button>
            <button
              type="button"
              aria-pressed={!p.code && !previewing}
              onClick={() => {
                p.onCode(false);
                preview(false);
              }}
            >
              <SquareTerminal size={14} aria-hidden /> Terminal
            </button>
            <button type="button" aria-pressed={p.code} aria-label="Code" title="Files, git history and ghost mode" onClick={() => p.onCode(true)}>
              <FileCode2 size={14} aria-hidden /> Code
            </button>
          </div>
        ) : (
          <div className="segmented" role="group" aria-label="View">
            <button
              type="button"
              aria-pressed={!p.code && s.view === 'chat'}
              onClick={() => {
                p.onCode(false);
                p.onView('chat');
              }}
            >
              <MessageSquare size={14} aria-hidden /> Chat
            </button>
            <button
              type="button"
              aria-pressed={!p.code && s.view === 'terminal'}
              onClick={() => {
                p.onCode(false);
                p.onView('terminal');
              }}
            >
              <SquareTerminal size={14} aria-hidden /> Terminal
            </button>
            <button type="button" aria-pressed={p.code} aria-label="Code" title="Files, git history and ghost mode" onClick={() => p.onCode(true)}>
              <FileCode2 size={14} aria-hidden /> Code
            </button>
          </div>
        )}
        <button type="button" className="icon-btn boxed" aria-label="Git" aria-pressed={p.showGit} onClick={p.onToggleGit}>
          <GitBranch size={16} aria-hidden />
        </button>
        <button type="button" className="icon-btn boxed" aria-label="Appearance" title="Appearance" onClick={p.onAppearance}>
          <Palette size={16} aria-hidden />
        </button>
      </header>

      {p.code && !p.gitRead ? (
        <div className="bench-empty mist">Reading the project…</div>
      ) : p.code ? (
        <Workbench
          key={p.git?.root ?? s.cwd}
          base={p.git?.root ?? s.cwd}
          git={p.git}
          intent={p.codeIntent}
          onError={p.onError}
          onNotify={p.onNotify}
        />
      ) : s.view === 'chat' ? (
        <ChatThread key={s.id} {...p} />
      ) : previewing ? (
        <CloudPreview key={s.id} session={s} back={back} onError={p.onError} />
      ) : (
        <div className="term-wrap">
          <TerminalView id={s.id} />
        </div>
      )}
    </>
  );
}

// What was typed but not sent, per session, kept while switching between them.
const drafts = new Map<string, { text: string; images: Attachment[] }>();

function ChatThread(p: Props) {
  const s = p.session;
  const items = useMemo(() => toItems(p.events), [p.events]);
  const blocks = useMemo(() => toBlocks(items), [items]);
  const todos = useMemo(() => latestTodos(items), [items]);
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const [away, setAway] = useState(false);
  const [draft, setDraftState] = useState(() => drafts.get(s.id)?.text ?? '');
  const [images, setImagesState] = useState<Attachment[]>(() => drafts.get(s.id)?.images ?? []);
  const [shown, setShown] = useState<string | null>(null);
  const [commands, setCommands] = useState<SlashCommand[]>([]);
  const box = useRef<HTMLTextAreaElement>(null);
  const working = s.status === 'working';

  useEffect(() => {
    let live = true;
    void commandsFor(s.cwd).then((c) => live && setCommands(c));
    return () => {
      live = false;
    };
  }, [s.cwd]);

  const setDraft = (text: string) => {
    setDraftState(text);
    drafts.set(s.id, { text, images });
  };
  const setImages = (a: Attachment[]) => {
    setImagesState(a);
    drafts.set(s.id, { text: draft, images: a });
  };

  const toBottom = (smooth = false) => {
    const el = scroller.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? 'smooth' : 'auto' });
    pinned.current = true;
    setAway(false);
  };

  useLayoutEffect(() => {
    if (pinned.current) toBottom();
  }, [items.length, s.status, todos?.length]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    box.current?.focus();
  }, []);

  const ready = !!draft.trim() || images.length > 0;
  const send = (typed?: string) => {
    const text = (typed ?? draft).trim();
    if (!text && !images.length) return;
    const done = () => {
      setDraftState('');
      setImagesState([]);
      drafts.delete(s.id);
    };
    const route = images.length ? { kind: 'chat' as const } : routeSlash(text, commands);
    switch (route.kind) {
      case 'clear':
        done();
        p.onClear();
        return;
      case 'model':
        done();
        p.onConfigure(route.model, null);
        return;
      case 'terminal':
        done();
        p.onTerminalCommand(text);
        return;
    }
    p.onSend(text, images);
    done();
    toBottom();
  };
  const reuse = (text: string) => {
    setDraft(text);
    requestAnimationFrame(() => box.current?.focus());
  };
  const mode = s.permissionMode || 'default';
  const lastBlock = blocks[blocks.length - 1];

  return (
    <>
      <div
        className="thread"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget;
          const near = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
          pinned.current = near;
          if (away === near) setAway(!near);
        }}
      >
        <div className="thread-inner">
          {blocks.map((b) => (
            <BlockView
              key={b.key}
              block={b}
              live={working && b === lastBlock}
              cwd={s.cwd}
              onAllow={p.onAllow}
              onTerminal={() => p.onView('terminal')}
              onImage={setShown}
              onReuse={reuse}
            />
          ))}
          {working && <LiveStatus items={items} />}
        </div>
      </div>
      <div className="chat-foot">
        {away && (
          <button type="button" className="jump" aria-label="Latest" title="Latest" onClick={() => toBottom(true)}>
            <ArrowDown size={16} aria-hidden />
          </button>
        )}
        {todos && todos.length > 0 && <PlanCard todos={todos} working={working} />}
        <Composer
          id="composer-chat"
          ref={box}
          label="Message"
          placeholder="Reply"
          rows={2}
          value={draft}
          onChange={setDraft}
          images={images}
          onImages={setImages}
          onError={p.onError}
          cwd={s.cwd}
          commands={commands}
          onSubmit={send}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              send();
            } else if (e.key === 'Tab' && e.shiftKey) {
              e.preventDefault();
              p.onConfigure(null, nextMode(mode));
            } else if (e.key === 'Escape' && working) {
              p.onStop();
            }
          }}
        >
          <Picker
            label="Model"
            variant="foot"
            placement="up"
            value={modelChoice(s.model)}
            display={s.model ? modelName(s.model) : 'Default'}
            choices={MODELS}
            onChange={(v) => p.onConfigure(v, null)}
          />
          <Picker
            label="Permissions"
            variant="foot"
            placement="up"
            value={mode}
            choices={MODES}
            onChange={(v) => p.onConfigure(null, v)}
            foot={<><kbd>Shift</kbd>+<kbd>Tab</kbd></>}
          />
          <div style={{ flex: 1 }} />
          {working && !ready ? (
            <button type="button" className="send-btn stop-btn" aria-label="Stop" onClick={p.onStop}>
              <Square size={14} aria-hidden />
            </button>
          ) : (
            <button type="button" className="send-btn" aria-label="Send" disabled={!ready} onClick={() => send()}>
              <ArrowUp size={16} strokeWidth={2.25} aria-hidden />
            </button>
          )}
        </Composer>
      </div>
      {shown && <Lightbox url={shown} onClose={() => setShown(null)} />}
    </>
  );
}

/// The line under a turn in progress: how long, and what it is doing.
function LiveStatus({ items }: { items: Item[] }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, []);
  const start = turnStart(items);
  const tool = runningTool(items);
  const doing = tool ? `${tool.name} ${toolSubject(tool.name, tool.input, '')}`.trim() : 'Working';
  return (
    <div className="working-row" role="status">
      <span className="pulse" aria-hidden>
        <span />
        <span />
        <span />
      </span>
      <span className="ellipsis">{doing}</span>
      {start && <span className="faint">{fmtElapsed(now - start)}</span>}
      <span className="faint working-esc"><kbd>Esc</kbd></span>
    </div>
  );
}

/// The plan the last TodoWrite set, pinned over the message box.
function PlanCard({ todos, working }: { todos: Todo[]; working: boolean }) {
  const done = todos.filter((t) => t.status === 'completed').length;
  const all = done === todos.length;
  const [open, setOpen] = useState(!all);
  useEffect(() => {
    if (all && !working) setOpen(false);
  }, [all, working]);
  const now = todos.find((t) => t.status === 'in_progress');
  return (
    <div className="plan">
      <button type="button" className="plan-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <ListTodo size={15} aria-hidden />
        <span className="plan-title">Plan</span>
        <span className="plan-count">{done}/{todos.length}</span>
        <span className="plan-bar" aria-hidden>
          <span style={{ width: `${(done / todos.length) * 100}%` }} />
        </span>
        {!open && now && <span className="plan-now ellipsis">{now.activeForm || now.content}</span>}
        <ChevronRight size={14} className="plan-chev" aria-hidden />
      </button>
      {open && (
        <ul className="plan-list">
          {todos.map((t, i) => (
            <li key={i} className={`todo ${t.status}`}>
              {t.status === 'completed' ? (
                <SquareCheck size={15} aria-hidden />
              ) : t.status === 'in_progress' ? (
                <CircleDot size={15} aria-hidden />
              ) : (
                <Circle size={15} aria-hidden />
              )}
              <span>{t.status === 'in_progress' && t.activeForm ? t.activeForm : t.content}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Lightbox({ url, onClose }: { url: string; onClose: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [onClose]);
  return (
    <div className="lightbox" role="dialog" aria-label="Image" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <img src={url} alt="" />
      <button type="button" className="icon-btn boxed lightbox-x" aria-label="Close" onClick={onClose}>
        <X size={17} aria-hidden />
      </button>
    </div>
  );
}

function CopyButton({ text, label = 'Copy' }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      className="act"
      aria-label={label}
      title={label}
      onClick={async () => {
        if (await copyText(text)) {
          setDone(true);
          window.setTimeout(() => setDone(false), 1400);
        }
      }}
    >
      {done ? <Check size={14} aria-hidden /> : <Copy size={14} aria-hidden />}
    </button>
  );
}

/// A fenced block with its language and a copy button over it.
function CodeBlock({ children }: { children?: ReactNode }) {
  const pre = useRef<HTMLPreElement>(null);
  const child = Array.isArray(children) ? children[0] : children;
  const cls = (child as { props?: { className?: string } } | undefined)?.props?.className ?? '';
  const lang = /language-([\w+#.-]+)/.exec(cls)?.[1] ?? '';
  return (
    <div className="code">
      <div className="code-head">
        <span>{lang || 'text'}</span>
        <CopyLive target={pre} />
      </div>
      <pre ref={pre}>{children}</pre>
    </div>
  );
}

function CopyLive({ target }: { target: React.RefObject<HTMLPreElement | null> }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      className="act"
      aria-label="Copy code"
      title="Copy"
      onClick={async () => {
        if (target.current && (await copyText(target.current.innerText))) {
          setDone(true);
          window.setTimeout(() => setDone(false), 1400);
        }
      }}
    >
      {done ? <Check size={13} aria-hidden /> : <Copy size={13} aria-hidden />}
    </button>
  );
}

const MD: Components = { pre: ({ children }) => <CodeBlock>{children}</CodeBlock> };

interface ItemProps {
  cwd: string;
  onAllow: (r: string) => void;
  onTerminal: () => void;
  onImage: (url: string) => void;
  onReuse: (text: string) => void;
}

function BlockView({ block, live, ...rest }: ItemProps & { block: Block; live: boolean }) {
  if (block.kind === 'steps') return <Steps items={block.items} live={live} cwd={rest.cwd} />;
  return <ItemView item={block} live={live} {...rest} />;
}

/// A run of tool calls as one line that opens: open while it runs, folded
/// once the turn moves on.
function Steps({ items, live, cwd }: { items: Array<ToolItem | ThinkingItem>; live: boolean; cwd: string }) {
  const [open, setOpen] = useState<boolean | null>(null);
  const shown = open ?? live;
  const failed = items.filter((i) => i.kind === 'tool' && i.result?.isError).length;
  const files = editedFiles(items);
  const running = items.some((i) => i.kind === 'tool' && !i.result);
  return (
    <div className="steps">
      <button type="button" className="steps-head" aria-expanded={shown} onClick={() => setOpen(!shown)}>
        {running ? <LoaderCircle size={14} className="spin" aria-hidden /> : <ChevronRight size={14} className="steps-chev" aria-hidden />}
        <span className="steps-sum">{stepsSummary(items)}</span>
        {failed > 0 && <span className="steps-bad">{failed} failed</span>}
      </button>
      {!shown && files.length > 0 && (
        <div className="file-chips">
          {files.map((f) => (
            <span key={f.path} className="file-chip">
              <Pencil size={11} aria-hidden />
              {relative(f.path, cwd).split('/').pop()}
              <span className="add">+{f.added}</span>
              <span className="del">−{f.removed}</span>
            </span>
          ))}
        </div>
      )}
      {shown && (
        <div className="steps-body">
          {items.map((it) => (it.kind === 'thinking' ? <Thinking key={it.key} text={it.text} /> : <ToolCard key={it.key} item={it} cwd={cwd} />))}
        </div>
      )}
    </div>
  );
}

function Thinking({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="thinking">
      <button type="button" className="thinking-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Brain size={14} aria-hidden />
        <span>Thinking</span>
        <ChevronRight size={13} className="steps-chev" aria-hidden />
      </button>
      {open && <div className="thinking-body">{text}</div>}
    </div>
  );
}

function ItemView({ item, live, cwd, onAllow, onTerminal, onImage, onReuse }: ItemProps & { item: Item; live: boolean }) {
  switch (item.kind) {
    case 'user':
      return (
        <div className="msg-user-wrap">
          {item.images.length > 0 && (
            <div className="msg-images">
              {item.images.map((url, i) => (
                <button key={i} type="button" className="msg-image" aria-label={`Image ${i + 1}`} onClick={() => onImage(url)}>
                  <img src={url} alt="" />
                </button>
              ))}
            </div>
          )}
          {item.text && <div className="msg-user">{item.text}</div>}
          {item.text && (
            <div className="acts">
              <CopyButton text={item.text} />
              <button type="button" className="act" aria-label="Edit and resend" title="Edit and resend" onClick={() => onReuse(item.text)}>
                <Undo2 size={14} aria-hidden />
              </button>
            </div>
          )}
        </div>
      );
    case 'note':
      return (
        <div className="msg-note">
          <GitFork size={13} aria-hidden />
          {item.text}
        </div>
      );
    case 'text':
      return (
        <div className="msg-ai">
          <Sailboat size={18} color="var(--accent)" style={{ flex: 'none', marginTop: 3 }} aria-hidden />
          <div className="msg-ai-body">
            <div className="md">
              <ReactMarkdown remarkPlugins={[remarkGfm]} components={MD}>{item.text}</ReactMarkdown>
            </div>
            {!live && (
              <div className="acts left">
                <CopyButton text={item.text} />
              </div>
            )}
          </div>
        </div>
      );
    case 'thinking':
      return (
        <div className="steps solo">
          <Thinking text={item.text} />
        </div>
      );
    case 'tool':
      return <ToolCard item={item} cwd={cwd} />;
    case 'denials':
      return <Denials denials={item.denials} cwd={cwd} onAllow={onAllow} onTerminal={onTerminal} />;
    case 'meta':
      return item.durationMs != null || item.costUsd != null ? (
        <div className="turn-meta">
          {item.durationMs != null && <span>{fmtElapsed(item.durationMs)}</span>}
          {item.costUsd != null && item.costUsd > 0 && <span>${item.costUsd < 0.01 ? item.costUsd.toFixed(3) : item.costUsd.toFixed(2)}</span>}
        </div>
      ) : null;
    case 'error':
      return (
        <div className="msg-error">
          <CircleAlert size={16} style={{ flex: 'none', marginTop: 2 }} aria-hidden />
          {item.text}
        </div>
      );
  }
}

function toolIcon(name: string) {
  switch (name) {
    case 'Edit':
    case 'MultiEdit':
    case 'NotebookEdit':
      return <Pencil size={15} color="var(--accent)" aria-hidden />;
    case 'Write':
      return <FilePlus size={15} color="var(--accent)" aria-hidden />;
    case 'Read':
      return <FileText size={15} color="var(--mist)" aria-hidden />;
    case 'Bash':
    case 'PowerShell':
      return <SquareTerminal size={15} color="var(--mist)" aria-hidden />;
    case 'Grep':
    case 'Glob':
    case 'WebSearch':
      return <Search size={15} color="var(--mist)" aria-hidden />;
    case 'WebFetch':
      return <Globe size={15} color="var(--mist)" aria-hidden />;
    case 'TodoWrite':
      return <ListTodo size={15} color="var(--mist)" aria-hidden />;
    case 'Task':
    case 'Agent':
      return <Bot size={15} color="var(--mist)" aria-hidden />;
    default:
      return <FileText size={15} color="var(--mist)" aria-hidden />;
  }
}

function ToolCard({ item, cwd }: { item: ToolItem; cwd: string }) {
  const diff = editDiff(item.input);
  const [open, setOpen] = useState(!!diff);
  const r = item.result;
  const meta = !r ? 'running' : r.isError ? 'failed' : diff ? `+${diff.filter((l) => l.sign === '+').length} −${diff.filter((l) => l.sign === '-').length}` : summarize(r.text);
  const metaClass = !r ? '' : r.isError ? 'bad' : 'ok';
  const body = diff ? null : r?.text;
  return (
    <div className="tool">
      <button type="button" className="tool-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        {toolIcon(item.name)}
        <span className="tool-name">{item.name}</span>
        <span className="tool-subject">{toolSubject(item.name, item.input, cwd)}</span>
        <span className={`tool-meta ${metaClass}`}>{!r ? <LoaderCircle size={13} className="spin" aria-label="running" /> : meta}</span>
      </button>
      {open && (diff || body) && (
        <div className="tool-body">
          {diff ? (
            <div className="diff">
              {diff.map((l, i) => (
                <div key={i} className={l.sign === '+' ? 'add' : l.sign === '-' ? 'del' : ''}>
                  <span className="sign" aria-hidden>{l.sign}</span>
                  <span>{l.text}</span>
                </div>
              ))}
            </div>
          ) : (
            <pre className="tool-out">{clip(body ?? '')}</pre>
          )}
        </div>
      )}
    </div>
  );
}

function summarize(text: string): string {
  const lines = text.split('\n').filter((l) => l.trim()).length;
  return lines <= 1 ? (text.trim().slice(0, 40) || 'done') : `${lines} lines`;
}

function clip(text: string): string {
  const lines = text.split('\n');
  return lines.length > 400 ? lines.slice(0, 400).join('\n') + `\n… ${lines.length - 400} more lines` : text;
}

function Denials({ denials, cwd, onAllow, onTerminal }: { denials: Denial[]; cwd: string; onAllow: (r: string) => void; onTerminal: () => void }) {
  const [done, setDone] = useState(false);
  if (done) return null;
  const rules = [...new Set(denials.map(allowRule))];
  return (
    <div className="ask">
      <div className="ask-title">
        <ShieldAlert size={17} color="var(--waiting)" aria-hidden />
        Blocked: {denials.length === 1 ? denials[0].tool : `${denials.length} tool calls`}
      </div>
      {denials.map((d, i) => (
        <code key={i}>{toolSubject(d.tool, d.input, cwd) || d.tool}</code>
      ))}
      <div className="btn-row">
        <button
          type="button"
          className="btn primary"
          onClick={() => {
            setDone(true);
            rules.forEach(onAllow);
          }}
        >
          Allow {rules.join(', ')}
        </button>
        <button type="button" className="btn" onClick={onTerminal}>
          Answer in terminal
        </button>
        <button type="button" className="btn quiet" onClick={() => setDone(true)}>
          Dismiss
        </button>
      </div>
    </div>
  );
}
