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
  allow: (id: string, tool: string) => invoke<void>('session_allow', { id, tool }),
  configure: (id: string, model: string | null, permissionMode: string | null) =>
    invoke<void>('session_configure', { id, model, permissionMode }),
  close: (id: string) => invoke<void>('session_close', { id }),
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
  settings: () => invoke<Settings>('settings_get'),
  saveSettings: (settings: Settings) => invoke<void>('settings_set', { settings }),
  importBackground: (path: string) => invoke<string>('background_import', { path }),
  customBackground: () => invoke<string | null>('background_custom'),
  updateState: () => invoke<UpdateState>('update_state'),
  updateCheck: () => invoke<UpdateState>('update_check'),
  updateInstall: () => invoke<UpdateState>('update_install'),
  restart: () => invoke<void>('app_restart'),
};

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
