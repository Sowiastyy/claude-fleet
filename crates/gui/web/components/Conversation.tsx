import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import {
  ArrowUp,
  Bot,
  CircleAlert,
  FilePlus,
  FileText,
  GitBranch,
  Globe,
  ImageIcon,
  ListTodo,
  MessageSquare,
  Pencil,
  Sailboat,
  Search,
  ShieldAlert,
  Square,
  SquareTerminal,
} from 'lucide-react';
import type { Event, GitState, Session, View } from '../api';
import { allowRule, editDiff, toItems, toolSubject, type Denial, type Item } from '../messages';
import { MODELS, MODES } from './Home';
import { statusLabel } from './Sidebar';
import { TerminalView } from './TerminalView';

interface Props {
  session: Session;
  events: Event[];
  git: GitState | null;
  showGit: boolean;
  onSend: (text: string) => void;
  onStop: () => void;
  onView: (v: View) => void;
  onAllow: (rule: string) => void;
  onConfigure: (model: string | null, mode: string | null) => void;
  onToggleGit: () => void;
  onBackground: () => void;
}

export function Conversation(p: Props) {
  const s = p.session;
  const branch = p.git?.branch;
  return (
    <>
      <header className="header">
        <span className={`dot ${s.status}`} aria-hidden />
        <div className="stack" style={{ flex: 'none', maxWidth: '40%' }}>
          <span className="ellipsis" style={{ fontWeight: 600, fontSize: 15 }}>{s.name}</span>
          <span className="small mist ellipsis">
            {s.cwd}
            {branch ? ` on ${branch}` : ''}
          </span>
        </div>
        <span className={`chip status-text ${s.status}`}>{statusLabel(s)}</span>
        <div style={{ flex: 1 }} />
        {s.view === 'chat' && s.status === 'working' && (
          <button type="button" className="btn" onClick={p.onStop} style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <Square size={13} aria-hidden /> Stop
          </button>
        )}
        <div className="segmented" role="group" aria-label="View">
          <button type="button" aria-pressed={s.view === 'chat'} onClick={() => p.onView('chat')}>
            <MessageSquare size={14} aria-hidden /> Chat
          </button>
          <button type="button" aria-pressed={s.view === 'terminal'} onClick={() => p.onView('terminal')}>
            <SquareTerminal size={14} aria-hidden /> Terminal
          </button>
        </div>
        <button type="button" className="icon-btn boxed" aria-label="Git" aria-pressed={p.showGit} onClick={p.onToggleGit}>
          <GitBranch size={16} aria-hidden />
        </button>
        <button type="button" className="icon-btn boxed" aria-label="Background" onClick={p.onBackground}>
          <ImageIcon size={16} aria-hidden />
        </button>
      </header>

      {s.view === 'chat' ? (
        <ChatThread {...p} />
      ) : (
        <div className="term-wrap">
          <TerminalView id={s.id} />
        </div>
      )}
    </>
  );
}

function ChatThread(p: Props) {
  const s = p.session;
  const items = useMemo(() => toItems(p.events), [p.events]);
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const [draft, setDraft] = useState('');
  const box = useRef<HTMLTextAreaElement>(null);

  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  }, [items.length, s.status]);
  useEffect(() => {
    pinned.current = true;
    box.current?.focus();
  }, [s.id]);

  const send = () => {
    const t = draft.trim();
    if (!t) return;
    p.onSend(t);
    setDraft('');
    pinned.current = true;
  };

  return (
    <>
      <div
        className="thread"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget;
          pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        }}
      >
        <div className="thread-inner">
          {items.map((it) => (
            <ItemView key={it.key} item={it} cwd={s.cwd} onAllow={p.onAllow} onTerminal={() => p.onView('terminal')} />
          ))}
          {s.status === 'working' && (
            <div className="working-row">
              <span className="pulse" aria-hidden>
                <span />
                <span />
                <span />
              </span>
              Working
            </div>
          )}
        </div>
      </div>
      <div className="chat-foot">
        <div className="composer">
          <label htmlFor="composer-chat" className="sr-only">Message</label>
          <textarea
            id="composer-chat"
            ref={box}
            rows={2}
            placeholder="Reply"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                send();
              } else if (e.key === 'Escape' && s.status === 'working') {
                p.onStop();
              }
            }}
          />
          <div className="composer-row">
            <label>
              <span className="sr-only">Model</span>
              <select className="foot-select" value={MODELS.some(([v]) => v === s.model) ? s.model : ''} onChange={(e) => p.onConfigure(e.target.value, null)}>
                {MODELS.map(([v, l]) => (
                  <option key={v} value={v}>{v === '' && s.model ? s.model : l}</option>
                ))}
              </select>
            </label>
            <label>
              <span className="sr-only">Permissions</span>
              <select className="foot-select" value={s.permissionMode || 'default'} onChange={(e) => p.onConfigure(null, e.target.value)}>
                {MODES.map(([v, l]) => (
                  <option key={v} value={v}>{l}</option>
                ))}
              </select>
            </label>
            <div style={{ flex: 1 }} />
            {s.status === 'working' && !draft.trim() ? (
              <button type="button" className="send-btn stop-btn" aria-label="Stop" onClick={p.onStop}>
                <Square size={14} aria-hidden />
              </button>
            ) : (
              <button type="button" className="send-btn" aria-label="Send" disabled={!draft.trim()} onClick={send}>
                <ArrowUp size={16} strokeWidth={2.25} aria-hidden />
              </button>
            )}
          </div>
        </div>
      </div>
    </>
  );
}

function ItemView({ item, cwd, onAllow, onTerminal }: { item: Item; cwd: string; onAllow: (r: string) => void; onTerminal: () => void }) {
  switch (item.kind) {
    case 'user':
      return <div className="msg-user">{item.text}</div>;
    case 'text':
      return (
        <div className="msg-ai">
          <Sailboat size={18} color="var(--accent)" style={{ flex: 'none', marginTop: 3 }} aria-hidden />
          <div className="md">
            <ReactMarkdown remarkPlugins={[remarkGfm]}>{item.text}</ReactMarkdown>
          </div>
        </div>
      );
    case 'tool':
      return <ToolCard item={item} cwd={cwd} />;
    case 'denials':
      return <Denials denials={item.denials} cwd={cwd} onAllow={onAllow} onTerminal={onTerminal} />;
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

function ToolCard({ item, cwd }: { item: Extract<Item, { kind: 'tool' }>; cwd: string }) {
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
        <span className={`tool-meta ${metaClass}`}>{meta}</span>
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
