// RRouter dashboard: panel registry + built-in panels. No build step, no dependencies.
const $ = (sel, root = document) => root.querySelector(sel);
const api = async (path, opts = {}) => {
  const res = await fetch(`/api${path}`, {
    headers: { 'content-type': 'application/json' },
    ...opts,
    body: opts.body ? JSON.stringify(opts.body) : undefined,
  });
  const text = await res.text();
  let data = {};
  try { data = text ? JSON.parse(text) : {}; } catch { data = { error: { message: text } }; }
  if (!res.ok) throw new Error(data?.error?.message || `HTTP ${res.status}`);
  return data;
};

const el = (tag, attrs = {}, ...kids) => {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'onclick') node.onclick = v;
    else if (v === true) node.setAttribute(k, '');
    else if (v !== false && v != null) node.setAttribute(k, v);
  }
  for (const kid of kids.flat()) {
    if (kid == null || kid === false) continue;
    node.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return node;
};

async function table(panel, columns, load, toolbar = []) {
  panel.replaceChildren(el('div', { class: 'toolbar' }, toolbar));
  const body = el('tbody');
  panel.append(el('table', {}, el('thead', {}, el('tr', {}, columns.map((c) => el('th', {}, c.title)))), body));
  let rows = [];
  try { rows = await load(); } catch (e) { panel.append(el('div', { class: 'muted' }, `error: ${e.message}`)); return; }
  for (const row of rows) {
    body.append(el('tr', {}, columns.map((c) => el('td', {}, c.render ? c.render(row) : row[c.key] ?? ''))));
  }
  if (!rows.length) body.append(el('tr', {}, el('td', { class: 'muted', colspan: columns.length }, 'nothing here yet')));
  return rows;
}

const input = (placeholder, value = '') => el('input', { placeholder, value });
const button = (label, onclick, cls = '') => el('button', { class: cls, onclick }, label);

// ---------- OAuth sign-in ----------
const oauthStatus = () => el('div', { class: 'oauth-status', role: 'status', 'aria-live': 'polite', tabindex: '-1' });
const errorBlock = (error, retry) => el('div', { class: 'error' }, error.message, retry ? el('div', { class: 'toolbar' }, button('Retry', retry)) : null);
async function startLogin(provider, slot, onDone) {
  const status = oauthStatus();
  slot.replaceChildren(status);
  status.focus({ preventScroll: true });
  status.append(el('div', { class: 'muted' }, `starting ${provider} sign-in…`));
  let started;
  try {
    started = await api(`/oauth/${provider}/start`, { method: 'POST' });
  } catch (err) {
    status.replaceChildren(errorBlock(err, () => startLogin(provider, slot, onDone)));
    return;
  }

  const { id, challenge } = started;
  let timer;
  let countdown;
  let completed = false;
  let stopped = false;
  const stopTimer = () => { clearInterval(timer); clearInterval(countdown); timer = countdown = undefined; };
  const cancel = async () => {
    stopTimer();
    try {
      await api(`/oauth/${provider}/${id}`, { method: 'DELETE' });
      slot.replaceChildren();
    } catch (err) {
      status.replaceChildren(errorBlock(err, cancel));
    }
  };

  if (challenge.kind === 'pkce') {
    const startedAt = Date.now();
    const progress = el('div', { class: 'muted' });
    const paint = () => { progress.textContent = `waiting for you to finish in the browser… elapsed ${Math.floor((Date.now() - startedAt) / 1000)}s`; };
    const finishButton = button('I authorized — continue', () => finish(), 'primary');
    status.replaceChildren(
      el('div', { class: 'card' },
        el('div', {}, `Authorize ${provider} in your browser.`),
        progress,
        el('div', { class: 'toolbar' },
          el('a', { href: challenge.authorize_url, target: '_blank', rel: 'noopener' }, 'Open authorization page'),
          finishButton,
          button('Cancel', cancel, 'danger'))),
    );
    paint();
    timer = setInterval(async () => {
      if (stopped) return;
      paint();
      try {
        const current = await api(`/oauth/${provider}/status/${id}`);
        if (!current.pending) {
          stopped = true;
          stopTimer();
          status.replaceChildren(errorBlock(new Error('this sign-in expired; start a new one'), () => startLogin(provider, slot, onDone)));
        }
      } catch (err) {
        stopped = true;
        stopTimer();
        status.replaceChildren(errorBlock(err, () => startLogin(provider, slot, onDone)));
      }
    }, 1000);
  } else {
    let remaining = Number(challenge.expires_in) || 0;
    const progress = el('div', { class: 'muted' });
    const code = el('div', { class: 'secret user-code', 'aria-label': 'Device authorization code' }, challenge.user_code);
    const paint = () => { progress.textContent = `waiting for authorization… ${Math.max(0, remaining)}s remaining`; };
    const copy = async () => {
      try { await navigator.clipboard.writeText(challenge.user_code); progress.textContent = 'code copied; waiting for authorization…'; }
      catch { progress.textContent = 'copy unavailable; select the code to copy it'; }
    };
    status.replaceChildren(
      el('div', { class: 'card' },
        el('div', {}, 'Enter this code on the provider:'),
        code,
        el('div', { class: 'toolbar' },
          button('Copy code', copy),
          el('a', { href: challenge.verification_uri, target: '_blank', rel: 'noopener' }, 'Open verification page'),
          button('Cancel', cancel, 'danger')),
        progress),
    );
    paint();
    countdown = setInterval(() => {
      remaining -= 1;
      paint();
      if (remaining <= 0) { clearInterval(countdown); countdown = undefined; }
    }, 1000);
    finish();
  }

  async function finish() {
    if (completed) return;
    completed = true;
    stopped = true;
    if (challenge.kind === 'pkce') stopTimer();
    const progress = el('div', { class: 'muted' }, 'waiting for provider…');
    status.append(progress);
    try {
      const out = await api(`/oauth/${provider}/complete`, { method: 'POST', body: { id } });
      stopTimer();
      status.replaceChildren(el('div', { class: 'card ok' },
        `Connected ${out.provider}${out.name ? ` as ${out.name}` : ''}`),
        out.warning ? el('div', { class: 'error' }, out.warning) : null);
      await onDone();
    } catch (err) {
      stopTimer();
      status.replaceChildren(errorBlock(err, () => startLogin(provider, slot, onDone)));
    }
  }
}

async function oauthProviders(panel, connect, loginSlot, refresh) {
  try {
    const providers = (await api('/oauth/providers')).data ?? [];
    // Spread the buttons: replaceChildren only flattens one level, so an array
    // argument would stringify each element into "[object HTMLButtonElement]".
    connect.replaceChildren(
      el('span', { class: 'muted' }, 'sign in:'),
      ...providers.map((provider) =>
        button(provider.display, () => startLogin(provider.id, loginSlot, refresh))),
    );
  } catch (err) {
    connect.replaceChildren(errorBlock(err, () => oauthProviders(panel, connect, loginSlot, refresh)));
  }
}

// ---------- built-in panels ----------
const builtins = {
  async connections(panel) {
    const loginSlot = el('div');
    const connectionError = el('div');
    const refresh = () => render(panel.parentElement, current);
    const addConnection = async (e) => {
      const [provider, authType, name, token] = e.target.parentElement.querySelectorAll('input');
      const credentials = authType.value === 'api_key' ? { api_key: token.value } : { access_token: token.value };
      try {
        await api('/connections', { method: 'POST', body: {
          provider: provider.value, auth_type: authType.value, name: name.value, credentials,
        } });
        connectionError.replaceChildren();
        refresh();
      } catch (err) {
        connectionError.replaceChildren(errorBlock(err, () => addConnection(e)));
      }
    };
    const form = el('div', { class: 'toolbar' },
      input('provider e.g. openai', 'openai'), input('auth_type', 'api_key'),
      input('name'), input('api_key / access_token'),
      button('add', addConnection, 'primary'));
    const connect = el('div', { class: 'toolbar' }, el('span', { class: 'muted' }, 'loading sign-in providers…'));
    await table(panel, [
      { title: 'provider', key: 'provider' },
      { title: 'name', key: 'name' },
      { title: 'auth', key: 'auth_type' },
      { title: 'priority', key: 'priority' },
      { title: 'tokens', render: (r) => [r.has_access_token && 'access', r.has_refresh_token && 'refresh', r.has_api_key && 'key'].filter(Boolean).join(' ') || '—', },
      { title: 'state', render: (r) => (r.is_active ? 'active' : `off (${r.backoff_level})`) },
      { title: '', render: (r) => el('span', {},
        button('test', async () => {
          try { connectionError.replaceChildren(el('div', { class: 'card' }, JSON.stringify(await api(`/connections/${r.id}/test`, { method: 'POST' })))); }
          catch (err) { connectionError.replaceChildren(errorBlock(err, () => render(panel.parentElement, current))); }
        }),
        button('delete', async () => {
          try { await api(`/connections/${r.id}`, { method: 'DELETE' }); refresh(); }
          catch (err) { connectionError.replaceChildren(errorBlock(err, () => render(panel.parentElement, current))); }
        }, 'danger')) },
    ], async () => (await api('/connections')).data ?? []);
    panel.prepend(el('div', { class: 'toolbar' }, form), connect, loginSlot, connectionError);
    oauthProviders(panel, connect, loginSlot, refresh);
  },

  async keys(panel) {
    await table(panel, [
      { title: 'name', key: 'name' },
      { title: 'id', key: 'id' },
      { title: 'created', render: (r) => new Date(r.created_at * 1000).toLocaleString() },
      { title: '', render: (r) => button('delete', async () => { await api(`/keys/${r.id}`, { method: 'DELETE' }); render(panel.parentElement, current); }, 'danger') },
    ], async () => (await api('/keys')).data ?? [], [
      input('name'),
      button('create key', async (e) => {
        const name = e.target.parentElement.querySelector('input').value;
        const out = await api('/keys', { method: 'POST', body: { name } });
        panel.prepend(el('div', { class: 'card' }, 'store this once: ', el('span', { class: 'secret' }, out.key)));
      }, 'primary'),
    ]);
  },

  async aliases(panel) {
    await table(panel, [
      { title: 'alias', key: 'alias' },
      { title: 'target', key: 'target' },
      { title: '', render: (r) => button('delete', async () => { await api(`/aliases?alias=${encodeURIComponent(r.alias)}`, { method: 'DELETE' }); render(panel.parentElement, current); }, 'danger') },
    ], async () => (await api('/aliases')).data ?? [], [
      input('alias'), input('provider/model'),
      button('save', async (e) => {
        const [alias, target] = e.target.parentElement.querySelectorAll('input');
        await api('/aliases', { method: 'POST', body: { alias: alias.value, target: target.value } });
        render(panel.parentElement, current);
      }, 'primary'),
    ]);
  },

  async combos(panel) {
    await table(panel, [
      { title: 'name', key: 'name' },
      { title: 'models', render: (r) => r.models.join(' → ') },
      { title: '', render: (r) => button('delete', async () => { await api(`/combos?name=${encodeURIComponent(r.name)}`, { method: 'DELETE' }); render(panel.parentElement, current); }, 'danger') },
    ], async () => (await api('/combos')).data ?? [], [
      input('name'), input('m1, m2, m3'),
      button('save', async (e) => {
        const [name, models] = e.target.parentElement.querySelectorAll('input');
        await api('/combos', { method: 'POST', body: { name: name.value, models: models.value.split(',').map((m) => m.trim()).filter(Boolean) } });
        render(panel.parentElement, current);
      }, 'primary'),
    ]);
  },

  async usage(panel) {
    panel.replaceChildren(el('h3', {}, 'summary'));
    await table(panel, [
      { title: 'provider', key: 'provider' },
      { title: 'model', key: 'model' },
      { title: 'requests', key: 'requests' },
      { title: 'errors', key: 'errors' },
      { title: 'in', key: 'input_tokens' },
      { title: 'out', key: 'output_tokens' },
      { title: 'avg ms', key: 'avg_latency_ms' },
    ], async () => (await api('/usage/summary')).data ?? []);
    panel.append(el('h3', {}, 'recent'));
    await table(panel, [
      { title: 'when', render: (r) => new Date(r.ts * 1000).toLocaleTimeString() },
      { title: 'provider', key: 'provider' },
      { title: 'model', key: 'model' },
      { title: 'status', key: 'status' },
      { title: 'ms', key: 'latency_ms' },
      { title: 'stream', render: (r) => (r.streamed ? 'yes' : 'no') },
    ], async () => (await api('/usage?limit=50')).data ?? []);
  },

  async status(panel) {
    const health = await (await fetch('/health')).json();
    const system = await api('/system');
    panel.replaceChildren(el('div', { class: 'card' },
      el('div', {}, `version ${health.version} · uptime ${health.uptime_secs}s`),
      el('div', { class: 'muted' }, `connections ${system.counts.connections} (${system.counts.active_connections} active) · keys ${system.counts.api_keys} · aliases ${system.counts.aliases} · combos ${system.counts.combos}`),
      el('div', { class: 'muted' }, `providers: ${system.counts.providers.join(', ') || 'none'}`),
      el('div', { class: 'muted' }, `require_api_key=${system.require_api_key} remote_admin=${system.allow_remote_admin} ext_assets=${system.static_assets}`)));
  },
};

// ---------- shell ----------
let panels = [];
let current = null;

function render(panel, id) {
  current = id;
  for (const tab of document.querySelectorAll('.tab')) {
    tab.setAttribute('aria-selected', String(tab.dataset.id === id));
  }
  panel.replaceChildren(el('div', { class: 'muted' }, 'loading…'));
  const def = panels.find((p) => p.id === id);
  if (!def) return;
  if (def.kind === 'extension') return renderExtension(panel, def);
  return (builtins[def.id] || builtins.status)(panel);
}

function renderExtension(panel, def) {
  const frame = el('iframe', { class: 'ext', sandbox: 'allow-scripts allow-forms', src: def.url });
  panel.replaceChildren(frame);
  const token = `${def.id}:${Date.now()}`;
  frame.onload = () => frame.contentWindow.postMessage({ type: 'rrouter:panel-token', token, panel: def.id }, new URL(def.url, location.origin).origin);
}

async function boot() {
  const [registry, settings] = await Promise.all([api('/ui/panels'), api('/settings')]);
  const brand = registry.brand ?? settings ?? {};
  if (brand.brand_name) { $('#brand').textContent = brand.brand_name; document.title = brand.brand_name; }
  if (brand.brand_color) document.documentElement.style.setProperty('--accent', brand.brand_color);
  if (brand.footer_text) $('#footer').textContent = brand.footer_text;
  if (brand.logo_url) $('#brand').prepend(el('img', { src: brand.logo_url, height: '16', alt: '' }));

  panels = registry.data ?? [];
  const tabs = $('#tabs');
  const panel = $('#panel');
  tabs.replaceChildren(...panels.map((p) => el('button', {
    class: 'tab', role: 'tab', 'data-id': p.id,
    onclick: () => render(panel, p.id),
  }, `${p.icon ?? ''} ${p.title}`.trim())));
  if (panels[0]) render(panel, panels[0].id);

  const pill = $('#health');
  const beat = async () => {
    try {
      const h = await (await fetch('/health')).json();
      pill.textContent = `ok ${h.uptime_secs}s`;
      pill.className = 'pill ok';
    } catch { pill.textContent = 'down'; pill.className = 'pill bad'; }
  };
  beat();
  setInterval(beat, 5000);
}

boot().catch((e) => {
  $('#panel').textContent = `dashboard error: ${e.message}`;
});
