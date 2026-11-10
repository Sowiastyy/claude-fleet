// A stand-in for the Tauri bridge, so the window's page can be opened in a
// plain browser with made-up data: `npm run preview:ui`, then
// http://localhost:1421/preview/ (add ?open=1 for a conversation,
// ?open=1&git=1 for the git panel, ?accounts=1, ?bg=1, ?open=4 for the cloud
// session, ?type=/co to type into the message box; ?open=1&code=1 for the
// code view, with &find=total.ts to open a file, &ghost=1 for ghost mode,
// &typed=1 to type a little of it, &hist=1 for the history, &diff=1 for a
// commit's diff).
(() => {
  const q = new URLSearchParams(location.search);
  if (q.get('git') === '1') localStorage.setItem('fleet.git', '1');
  else localStorage.removeItem('fleet.git');
  localStorage.setItem('fleet.where', q.get('where') || 'local');
  const now = Date.now();
  const sessions = [
    { id: 's1', name: 'web-checkout', cwd: 'C:\\code\\smartwood-web', conversation: 'a', view: 'chat', status: 'working', waitingFor: '', model: 'claude-opus-5-5', permissionMode: 'default', startedAt: now - 840e3 },
    { id: 's2', name: 'orders-api', cwd: 'C:\\code\\smartwood-api', conversation: 'b', view: 'chat', status: 'waiting', waitingFor: 'permission', model: '', permissionMode: 'default', startedAt: now - 3720e3 },
    { id: 's3', name: 'claude-fleet', cwd: 'C:\\code\\claude-fleet', conversation: 'c', view: 'terminal', status: 'idle', waitingFor: '', model: '', permissionMode: 'acceptEdits', startedAt: now - 2280e3 },
    { id: 's4', name: 'smartwood-web', cwd: 'C:\\code\\smartwood-web', conversation: 'd', view: 'terminal', status: 'working', waitingFor: '', model: '', permissionMode: 'default', startedAt: now - 600e3, cloud: true, cloudId: 'cse_01HkV2m9', cloudUrl: 'https://claude.ai/code/cse_01HkV2m9', cloudTask: 'Upgrade the image pipeline to sharp 0.34' },
  ].map((s, i) => ({ cloud: false, cloudId: '', cloudUrl: '', cloudTask: '', alive: true, account: q.get('one') ? 'u1' : ['u1', 'u2', 'u1', 'u2'][i], ...s }));
  const cmd = (name, description, argumentHint = '', terminal = false, aliases = []) => ({ name, description, argumentHint, aliases, terminal });
  const log = [
    { type: 'fleet_user', text: 'The checkout total ignores the voucher when the cart has a mattress in it. Fix it and add a test.', images: ['data:image/svg+xml;base64,' + btoa('<svg xmlns="http://www.w3.org/2000/svg" width="480" height="300"><rect width="480" height="300" fill="#f4f1ea"/><rect x="24" y="24" width="432" height="40" rx="6" fill="#d9d3c4"/><rect x="24" y="84" width="280" height="18" rx="4" fill="#bdb5a3"/><rect x="24" y="114" width="200" height="18" rx="4" fill="#bdb5a3"/><rect x="320" y="230" width="136" height="46" rx="8" fill="#c0403f"/></svg>')] },
    { type: 'fleet_note', text: 'Forked from f51217cf, running elsewhere' },
    { type: 'assistant', message: { content: [{ type: 'thinking', thinking: 'The voucher is subtracted from the subtotal, then surcharges are added on top. A mattress adds a size surcharge, so the voucher misses it. Applying it last fixes the order.' }, { type: 'tool_use', id: 'p1', name: 'TodoWrite', input: { todos: [{ content: 'Find where the voucher is applied', status: 'completed', activeForm: 'Finding where the voucher is applied' }, { content: 'Apply the voucher after surcharges', status: 'completed', activeForm: 'Applying the voucher after surcharges' }, { content: 'Add a test for a cart with a mattress', status: 'in_progress', activeForm: 'Adding a test for a cart with a mattress' }, { content: 'Run the checkout tests', status: 'pending', activeForm: 'Running the checkout tests' }] } }] } },
    { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 'p1', content: 'Todos updated' }] } },
    { type: 'assistant', message: { content: [{ type: 'tool_use', id: 'g1', name: 'Grep', input: { pattern: 'applyVoucher' } }, { type: 'tool_use', id: 't1', name: 'Read', input: { file_path: 'C:\\code\\smartwood-web\\src\\checkout\\total.ts' } }, { type: 'tool_use', id: 't1b', name: 'Read', input: { file_path: 'C:\\code\\smartwood-web\\src\\checkout\\surcharges.ts' } }] } },
    { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 'g1', content: 'src/checkout/total.ts:4' }, { type: 'tool_result', tool_use_id: 't1', content: 'export function total(lines, voucher) {\n  ...\n}' }, { type: 'tool_result', tool_use_id: 't1b', content: '...' }] } },
    { type: 'assistant', message: { content: [{ type: 'tool_use', id: 't2', name: 'Edit', input: { file_path: 'C:\\code\\smartwood-web\\src\\checkout\\total.ts', old_string: 'const discounted = applyVoucher(subtotal, voucher);\nreturn discounted + surcharges(lines);', new_string: 'const priced = subtotal + surcharges(lines);\nreturn applyVoucher(priced, voucher);' } }] } },
    { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 't2', content: 'The file has been updated.' }] } },
    { type: 'assistant', message: { content: [{ type: 'text', text: 'The voucher is now applied **after** every surcharge:\n\n```ts\nconst priced = subtotal + surcharges(lines);\nreturn applyVoucher(priced, voucher);\n```\n\nNext I will add a test with a mattress in the cart.' }] } },
    { type: 'result', subtype: 'success', duration_ms: 48200, total_cost_usd: 0.214 },
    { type: 'fleet_user', text: 'Go on, and run the tests.', ts: Date.now() - 37000 },
    { type: 'assistant', message: { content: [{ type: 'tool_use', id: 'w1', name: 'Write', input: { file_path: 'C:\\code\\smartwood-web\\src\\checkout\\total.test.ts', content: 'import { total } from "./total";\n\ntest("voucher after mattress surcharge", () => {\n  expect(total(cart, voucher)).toBe(1710);\n});' } }] } },
    { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 'w1', content: 'File created' }] } },
    { type: 'assistant', message: { content: [{ type: 'tool_use', id: 't3', name: 'Bash', input: { command: 'npm test -- checkout' } }] } },
  ];
  const limits = (h, d) => ({ session: { pct: h, resetsInSecs: 5640, expired: false }, weekly: { pct: d, resetsInSecs: 518400, expired: false }, fetchedAgoSecs: 60 });
  const accounts = {
    current: 'u1',
    live: limits(53, 7),
    accounts: [
      { uuid: 'u1', email: 'me@example.com', name: 'Karton', org: '', plan: 'pro', needsLogin: false, current: true, limits: limits(53, 7) },
      { uuid: 'u2', email: 'work@example.com', name: 'Karton', org: 'Smartwood', plan: 'max', needsLogin: false, current: false, limits: { session: { pct: 91, resetsInSecs: null, expired: true }, weekly: { pct: 64, resetsInSecs: 172800, expired: false }, fetchedAgoSecs: 7200 } },
      { uuid: 'u3', email: 'old-team@example.com', name: '', org: '', plan: 'pro', needsLogin: true, current: false, limits: null },
    ],
  };
  const git = {
    root: 'C:\\code\\smartwood-web', branch: 'feat/checkout', upstream: 'origin/feat/checkout', ahead: 2, behind: 0,
    changes: [{ code: ' M', path: 'src/checkout/total.ts', added: 3, removed: 2 }, { code: '??', path: 'src/checkout/total.test.ts', added: 48, removed: 0 }],
    changesTotal: 2, added: 51, removed: 2,
    log: [
      { hash: 'a41c9e2aa', subject: 'Apply vouchers after mattress surcharges', agoSecs: 180, author: 'me' },
      { hash: '7d0b113bb', subject: 'Show the size surcharge on the cart line', agoSecs: 1500, author: 'me' },
      { hash: 'e93f04acc', subject: 'Merge branch main into feat/checkout', agoSecs: 7200, author: 'me' },
    ],
  };
  const totalTs = [
    "import { applyVoucher } from './voucher';",
    "import { surcharges } from './surcharges';",
    '',
    '/// What the customer pays: every surcharge first, then the voucher.',
    'export function total(lines: Line[], voucher?: Voucher): number {',
    '  const subtotal = lines.reduce((sum, l) => sum + l.price * l.qty, 0);',
    '  const priced = subtotal + surcharges(lines);',
    '  return applyVoucher(priced, voucher);',
    '}',
    '',
  ].join('\n');
  const commitDiff = [
    'diff --git a/src/checkout/total.ts b/src/checkout/total.ts',
    'index 3f1c2aa..9b04e1d 100644',
    '--- a/src/checkout/total.ts',
    '+++ b/src/checkout/total.ts',
    "@@ -3,8 +3,9 @@ import { surcharges } from './surcharges';",
    ' ',
    '-/// What the customer pays.',
    '+/// What the customer pays: every surcharge first, then the voucher.',
    ' export function total(lines: Line[], voucher?: Voucher): number {',
    '   const subtotal = lines.reduce((sum, l) => sum + l.price * l.qty, 0);',
    '-  const discounted = applyVoucher(subtotal, voucher);',
    '-  return discounted + surcharges(lines);',
    '+  const priced = subtotal + surcharges(lines);',
    '+  return applyVoucher(priced, voucher);',
    ' }',
  ];
  const answers = {
    sessions_list: sessions,
    session_log: log,
    foreign_sessions: [{ name: 'dashboard-7f', cwd: 'C:\\code\\dashboard', status: 'busy' }],
    accounts_state: accounts,
    recent_conversations: [
      { id: 'r1', cwd: 'C:\\code\\billing-api', project: 'billing-api', summary: 'Add a date filter to the orders view', agoSecs: 18000 },
      { id: 'r2', cwd: 'C:\\code\\pdf-tools', project: 'pdf-tools', summary: 'Batch print manager, build the spec', agoSecs: 172800 },
      { id: 'r3', cwd: 'C:\\code\\claude-fleet', project: 'claude-fleet', summary: 'Make the limit bar bigger and add resume', agoSecs: 259200 },
    ],
    recent_projects: ['C:\\code\\claude-fleet', 'C:\\code\\smartwood-web', 'C:\\code\\smartwood-api'],
    home_dir: 'C:\\Users\\me',
    files_list: ['src/checkout/total.ts', 'src/checkout/total.test.ts', 'src/checkout/surcharges.ts', 'src/cart/Cart.tsx', 'src/cart/voucher.ts', 'package.json', 'README.md'],
    git_state: git,
    settings_get: { background: 'harbor', customImage: null, newChatDim: 65, chatDim: 85, theme: q.get('theme') || 'harbor', accent: '' },
    background_custom: null,
    slash_commands: [
      cmd('compact', 'Free up context by summarizing the conversation so far', '<optional custom summarization instructions>'),
      cmd('clear', 'Start a new session with empty context; previous session stays on disk (resumable with /resume)', '', false, ['reset', 'new']),
      cmd('context', 'Show current context usage'),
      cmd('model', 'Set the AI model for Claude Code', '<model>'),
      cmd('init', 'Initialize a new CLAUDE.md file with codebase documentation'),
      cmd('code-review', 'Review the current diff for correctness bugs', '[level] [target]'),
      cmd('security-review', 'Complete a security review of the pending changes on the current branch'),
      cmd('usage', 'Show session cost, plan usage, and activity stats', '', false, ['cost', 'stats']),
      cmd('caveman:caveman-commit', '(caveman) Ultra-compressed commit message generator'),
      cmd('config', 'Set a setting by key', 'key=value'),
      cmd('login', 'Sign in to your Anthropic account', '', true),
      cmd('resume', 'Resume a previous conversation', '[conversation id or search term]', true),
      cmd('permissions', 'Manage allow and deny tool permission rules', '', true),
    ],
    cloud_recent: [
      { id: 'cse_01HkV2m9', url: 'https://claude.ai/code/cse_01HkV2m9', task: 'Upgrade the image pipeline to sharp 0.34', cwd: 'C:\\code\\smartwood-web', at: now - 600e3 },
      { id: 'session_011CUx9', url: 'https://claude.ai/code/session_011CUx9', task: 'Translate the invoice templates to Czech', cwd: 'C:\\code\\billing-api', at: now - 90000e3 },
    ],
    file_read: { text: totalTs, stamp: 1, binary: false, lossy: false },
    file_stamp: 1,
    file_write: 1,
    git_history: [
      { hash: 'a41c9e2aa', subject: 'Apply vouchers after mattress surcharges', agoSecs: 180, author: 'me', path: null },
      { hash: '7d0b113bb', subject: 'Show the size surcharge on the cart line', agoSecs: 1500, author: 'me', path: null },
      { hash: 'e93f04acc', subject: 'Merge branch main into feat/checkout', agoSecs: 7200, author: 'me', path: null },
      { hash: '51c0d7edd', subject: 'Add the voucher field to checkout', agoSecs: 90000, author: 'Ola', path: null },
    ],
    git_commit_info: {
      author: 'me', email: 'me@example.com', date: '2026-11-10 14:02',
      message: 'Apply vouchers after mattress surcharges\n\nA mattress adds a size surcharge after the voucher was taken off,\nso the voucher never covered it.',
      files: [{ path: 'src/checkout/total.ts', added: 3, removed: 3 }, { path: 'src/checkout/total.test.ts', added: 48, removed: 0 }],
    },
    git_diff: commitDiff,
    update_state: { current: '0.2.0', available: q.get('update') ? 'v0.2.9' : null, installing: false, installed: q.get('update') === 'ready', checking: false },
  };
  let next = 1;
  window.__TAURI_INTERNALS__ = {
    invoke: async (cmd) => (cmd in answers ? structuredClone(answers[cmd]) : cmd.startsWith('plugin:event') ? next++ : null),
    transformCallback: () => next++,
    unregisterCallback: () => {},
    convertFileSrc: (p) => p,
    metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  const click = (sel, delay) => setTimeout(() => document.querySelector(sel)?.click(), delay);
  if (q.get('open')) setTimeout(() => document.querySelectorAll('.session-btn')[Number(q.get('open')) - 1]?.click(), 800);
  if (q.get('type')) {
    setTimeout(() => {
      const el = document.querySelector('textarea');
      if (!el) return;
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(el, q.get('type'));
      el.focus();
      el.dispatchEvent(new Event('input', { bubbles: true }));
    }, 1800);
  }
  if (q.get('accounts') === '1') click('.account-btn', 800);
  if (q.get('code')) {
    click('[aria-label=Code]', 1300);
    const setValue = (el, v) => {
      const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, v);
      el.dispatchEvent(new Event('input', { bubbles: true }));
    };
    if (q.get('find')) {
      setTimeout(() => {
        const el = document.querySelector('.side-search input');
        if (!el) return;
        setValue(el, q.get('find'));
        setTimeout(() => el.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })), 150);
      }, 1900);
    }
    if (q.get('ghost')) click('.bench-bar button[title^="Ghost mode"]', q.get('diff') ? 3600 : 2700);
    if (q.get('typed')) {
      setTimeout(() => {
        const el = document.querySelector('.ghost-input');
        if (!el) return;
        el.focus();
        for (const ch of "import { applyVoucher } from './voucher';\nimport { surx") setValue(el, ch);
      }, 3300);
    }
    if (q.get('hist') || q.get('diff')) click('.bench-side-tabs button:nth-child(2)', 1900);
    if (q.get('diff')) {
      click('.hist-commit-main', 2400);
      click('.hist-detail .hist-file-main', 3000);
    }
  }
  if (q.get('click')) click(q.get('click'), 1500);
  if (q.get('bg') === '1') click(q.get('open') ?'[aria-label=Appearance]' : '.ghost-btn', 1200);
})();
