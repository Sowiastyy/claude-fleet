// A stand-in for the Tauri bridge, so the window's page can be opened in a
// plain browser with made-up data: `npm run preview:ui`, then
// http://localhost:1421/preview/ (add ?open=1 for a conversation,
// ?open=1&git=1 for the git panel, ?accounts=1, ?bg=1).
(() => {
  const q = new URLSearchParams(location.search);
  if (q.get('git') === '1') localStorage.setItem('fleet.git', '1');
  else localStorage.removeItem('fleet.git');
  const now = Date.now();
  const sessions = [
    { id: 's1', name: 'web-checkout', cwd: 'C:\\code\\smartwood-web', conversation: 'a', view: 'chat', status: 'working', waitingFor: '', model: 'claude-opus-5-5', permissionMode: 'default', startedAt: now - 840e3 },
    { id: 's2', name: 'orders-api', cwd: 'C:\\code\\smartwood-api', conversation: 'b', view: 'chat', status: 'waiting', waitingFor: 'permission', model: '', permissionMode: 'default', startedAt: now - 3720e3 },
    { id: 's3', name: 'claude-fleet', cwd: 'C:\\code\\claude-fleet', conversation: 'c', view: 'terminal', status: 'idle', waitingFor: '', model: '', permissionMode: 'acceptEdits', startedAt: now - 2280e3 },
  ];
  const log = [
    { type: 'fleet_user', text: 'The checkout total ignores the voucher when the cart has a mattress in it. Fix it and add a test.', images: ['data:image/svg+xml;base64,' + btoa('<svg xmlns="http://www.w3.org/2000/svg" width="480" height="300"><rect width="480" height="300" fill="#f4f1ea"/><rect x="24" y="24" width="432" height="40" rx="6" fill="#d9d3c4"/><rect x="24" y="84" width="280" height="18" rx="4" fill="#bdb5a3"/><rect x="24" y="114" width="200" height="18" rx="4" fill="#bdb5a3"/><rect x="320" y="230" width="136" height="46" rx="8" fill="#c0403f"/></svg>')] },
    { type: 'fleet_note', text: 'Forked from f51217cf, running elsewhere' },
    { type: 'assistant', message: { content: [{ type: 'text', text: 'The voucher is applied **before** the mattress size surcharge is added, so the discount comes off the old subtotal. I will apply it last, after every surcharge.' }, { type: 'tool_use', id: 't1', name: 'Read', input: { file_path: 'C:\\code\\smartwood-web\\src\\checkout\\total.ts' } }] } },
    { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 't1', content: 'export function total(lines, voucher) {\n  ...\n}' }] } },
    { type: 'assistant', message: { content: [{ type: 'tool_use', id: 't2', name: 'Edit', input: { file_path: 'C:\\code\\smartwood-web\\src\\checkout\\total.ts', old_string: 'const discounted = applyVoucher(subtotal, voucher);\nreturn discounted + surcharges(lines);', new_string: 'const priced = subtotal + surcharges(lines);\nreturn applyVoucher(priced, voucher);' } }] } },
    { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 't2', content: 'The file has been updated.' }] } },
    { type: 'assistant', message: { content: [{ type: 'tool_use', id: 't3', name: 'Bash', input: { command: 'npm test -- checkout' } }] } },
    { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 't3', is_error: true, content: 'Permission to use Bash has been denied.' }] } },
    { type: 'result', subtype: 'success', permission_denials: [{ tool_name: 'Bash', tool_input: { command: 'npm test -- checkout' } }] },
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
    git_state: git,
    settings_get: { background: 'harbor', customImage: null, newChatDim: 65, chatDim: 85, theme: q.get('theme') || 'harbor', accent: '' },
    background_custom: null,
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
  if (q.get('open') === '1') click('.session-btn', 800);
  if (q.get('accounts') === '1') click('.account-btn', 800);
  if (q.get('click')) click(q.get('click'), 1500);
  if (q.get('bg') === '1') click(q.get('open') === '1' ? '[aria-label=Appearance]' : '.ghost-btn', 1200);
})();
