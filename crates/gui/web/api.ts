// The commands the Rust side answers, and the events it sends.
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export type View = 'chat' | 'terminal';
export type Status = 'starting' | 'working' | 'idle' | 'waiting';

export interface Session {
  id: string;
  name: string;
  cwd: string;
  conversation: string;
  view: View;
  status: Status;
  waitingFor: string;
  model: string;
  permissionMode: string;
  startedAt: number;
  /// Runs in the cloud (`claude --cloud`): terminal only.
  cloud: boolean;
  /// Its id and claude.ai/code link, once Claude Code has said them.
  cloudId: string;
  cloudUrl: string;
  cloudTask: string;
  /// A plain shell in a terminal rather than Claude Code.
  shell: boolean;
  /// Its terminal process is running.
  alive: boolean;
  /// The account it runs as; empty for whatever `~/.claude` is signed in as.
  account: string;
}

/// A slash command Claude Code runs, as it describes it.
export interface SlashCommand {
  name: string;
  description: string;
  argumentHint: string;
  aliases: string[];
  /// Only the interactive terminal has it.
  terminal: boolean;
}

/// A cloud session seen here, to attach to again.
export interface CloudRecent {
  id: string;
  url: string;
  task: string;
  cwd: string;
  at: number;
}

export interface Foreign {
  name: string;
  cwd: string;
  status: string;
}

export interface LimitWindow {
  pct: number;
  resetsInSecs: number | null;
  expired: boolean;
}

export interface Limits {
  session: LimitWindow | null;
  weekly: LimitWindow | null;
  fetchedAgoSecs: number;
}

export interface Account {
  uuid: string;
  email: string;
  name: string;
  org: string;
  plan: string;
  needsLogin: boolean;
  current: boolean;
  limits: Limits | null;
}

export interface Accounts {
  current: string | null;
  accounts: Account[];
  live: Limits | null;
}

export interface Recent {
  id: string;
  cwd: string;
  project: string;
  summary: string;
  agoSecs: number;
}

export interface GitChange {
  code: string;
  path: string;
  added: number | null;
  removed: number | null;
}

export interface GitCommit {
  hash: string;
  subject: string;
  agoSecs: number;
  author: string;
}

export interface GitState {
  root: string;
  branch: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  changes: GitChange[];
  changesTotal: number;
  added: number;
  removed: number;
  log: GitCommit[];
}

/// A file opened in the code view.
export interface OpenedFile {
  text: string;
  /// When it last changed on disk, handed back on save.
  stamp: number;
  binary: boolean;
  /// Not UTF-8: shown, but saving would break it.
  lossy: boolean;
}

export interface HistoryEntry {
  hash: string;
  subject: string;
  agoSecs: number;
  author: string;
  /// In a file's own history, the name it had in this commit.
  path: string | null;
}

export interface CommitInfo {
  author: string;
  email: string;
  date: string;
  message: string;
  files: Array<{ path: string; added: number | null; removed: number | null }>;
}

/// What a diff is of.
export type DiffOf =
  | { kind: 'worktree'; path: string; untracked: boolean }
  | { kind: 'commit'; hash: string; path: string };

export interface Settings {
  background: string;
  customImage: string | null;
  newChatDim: number;
  chatDim: number;
  theme: string;
  accent: string;
}

/// An image on its way into a message.
export interface Attachment {
  id: string;
  name: string;
  mediaType: string;
  /// Base64, no `data:` prefix.
  data: string;
  /// The same image as a data URL, for showing it.
  url: string;
}

export interface NewSession {
  cwd: string;
  prompt?: string;
  model?: string;
  permissionMode?: string;
  resume?: string;
  terminal?: boolean;
  images?: Attachment[];
  /// A new cloud session for `prompt`, or with `resume` the existing one
  /// that id or claude.ai/code link names.
  cloud?: boolean;
  /// Pull this cloud session into a local terminal session.
  teleport?: string;
  /// A plain shell in the folder instead of Claude Code.
  shell?: boolean;
  /// Typed into the terminal once Claude Code is up.
  typeIn?: string;
  /// The account to run as; the signed-in one when left out.
  account?: string;
}

// The stream-json objects Claude Code prints, plus the fleet's own
// `fleet_user` and `fleet_error`. Loosely typed on purpose: it is Claude
// Code's format, not ours.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Event = Record<string, any>;

export const api = {
  sessions: () => invoke<Session[]>('sessions_list'),
  log: (id: string) => invoke<Event[]>('session_log', { id }),
  create: (req: NewSession) =>
    invoke<string>('session_create', {
      req: { ...req, images: (req.images ?? []).map(({ mediaType, data }) => ({ mediaType, data })) },
    }),
  send: (id: string, text: string, images: Attachment[] = []) =>
    invoke<void>('session_send', { id, text, images: images.map(({ mediaType, data }) => ({ mediaType, data })) }),
  interrupt: (id: string) => invoke<void>('session_interrupt', { id }),
  setView: (id: string, view: View, rows: number, cols: number) =>
    invoke<void>('session_set_view', { id, view, rows, cols }),
  setAccount: (id: string, account: string, rows: number, cols: number) =>
    invoke<void>('session_account', { id, account, rows, cols }),
  allow: (id: string, tool: string) => invoke<void>('session_allow', { id, tool }),
  configure: (id: string, model: string | null, permissionMode: string | null) =>
    invoke<void>('session_configure', { id, model, permissionMode }),
  close: (id: string) => invoke<void>('session_close', { id }),
  terminalCommand: (id: string, text: string, rows: number, cols: number) =>
    invoke<void>('session_terminal_command', { id, text, rows, cols }),
  clear: (id: string) => invoke<void>('session_clear', { id }),
  restartTerm: (id: string, rows: number, cols: number) => invoke<void>('session_restart', { id, rows, cols }),
  commands: (cwd: string) => invoke<SlashCommand[]>('slash_commands', { cwd }),
  cloudRecent: () => invoke<CloudRecent[]>('cloud_recent'),
  cloudForget: (id: string) => invoke<void>('cloud_forget', { id }),
  openUrl: (url: string) => invoke<void>('open_url', { url }),
  /// The cloud session's claude.ai page over the box at `bounds` (CSS
  /// pixels), as `account`; `seq` keeps a late call from undoing a newer one.
  cloudViewShow: (seq: number, account: string, url: string, bounds: Bounds, reload: boolean) =>
    invoke<void>('cloud_view_show', { seq, account, url, bounds, reload }),
  cloudViewHide: (seq: number) => invoke<void>('cloud_view_hide', { seq }),
  termWrite: (id: string, data: string) => invoke<void>('term_write', { id, data }),
  termResize: (id: string, rows: number, cols: number) => invoke<void>('term_resize', { id, rows, cols }),
  foreign: () => invoke<Foreign[]>('foreign_sessions'),
  accounts: () => invoke<Accounts>('accounts_state'),
  switchAccount: (uuid: string, carry: boolean) => invoke<string>('accounts_switch', { uuid, carry }),
  forgetAccount: (uuid: string) => invoke<void>('accounts_forget', { uuid }),
  addAccount: (cwd: string) => invoke<string>('accounts_add', { cwd }),
  recent: () => invoke<Recent[]>('recent_conversations'),
  projects: () => invoke<string[]>('recent_projects'),
  home: () => invoke<string>('home_dir'),
  files: (cwd: string) => invoke<string[]>('files_list', { cwd }),
  git: (cwd: string) => invoke<GitState | null>('git_state', { cwd }),
  commit: (cwd: string, message: string) => invoke<string>('git_commit', { cwd, message }),
  generate: (cwd: string) => invoke<string>('git_generate', { cwd }),
  push: (cwd: string) => invoke<void>('git_push', { cwd }),
  pull: (cwd: string) => invoke<void>('git_pull', { cwd }),
  fileRead: (base: string, path: string) => invoke<OpenedFile>('file_read', { base, path }),
  fileStamp: (base: string, path: string) => invoke<number>('file_stamp', { base, path }),
  /// Refused with "changed on disk" when `expected` no longer matches.
  fileWrite: (base: string, path: string, text: string, expected: number | null) =>
    invoke<number>('file_write', { base, path, text, expected }),
  history: (base: string, path: string | null, skip: number, limit: number) =>
    invoke<HistoryEntry[]>('git_history', { base, path, skip, limit }),
  commitInfo: (base: string, hash: string) => invoke<CommitInfo>('git_commit_info', { base, hash }),
  diff: (base: string, of: DiffOf) => invoke<string[]>('git_diff', { base, of }),
  settings: () => invoke<Settings>('settings_get'),
  saveSettings: (settings: Settings) => invoke<void>('settings_set', { settings }),
  importBackground: (path: string) => invoke<string>('background_import', { path }),
  customBackground: () => invoke<string | null>('background_custom'),
  updateState: () => invoke<UpdateState>('update_state'),
  updateCheck: () => invoke<UpdateState>('update_check'),
  updateInstall: () => invoke<UpdateState>('update_install'),
  restart: () => invoke<void>('app_restart'),
};

export interface Bounds {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface UpdateState {
  current: string;
  available: string | null;
  installing: boolean;
  installed: boolean;
  checking: boolean;
}

export function on<T>(name: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(name, (e) => handler(e.payload));
}

export function errorText(e: unknown): string {
  return typeof e === 'string' ? e : e instanceof Error ? e.message : String(e);
}
