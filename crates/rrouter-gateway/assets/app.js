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
// Providers whose sign-in RRouter implements. Kept here so the button list is
// explicit rather than a guess from the connection list.
const OAUTH_PROVIDERS = [
  { id: 'claude', label: 'Claude' },
  { id: 'codex', label: 'Codex' },
  { id: 'github', label: 'GitHub Copilot' },
];

/// Run one sign-in end to end, showing whatever the human must do.
async function startLogin(provider, slot, onDone) {
  slot.replaceChildren(el('div', { class: 'muted' }, `starting ${provider} sign-in…`));
  let started;
  try {
    started = await api(`/oauth/${provider}/start`, { method: 'POST' });
  } catch (err) {
    slot.replaceChildren(el('div', { class: 'error' }, `${provider}: ${err.message}`));
    return;
  }
  const { id, challenge } = started;

  if (challenge.kind === 'pkce') {
    slot.replaceChildren(
      el('div', { class: 'card' },
        el('div', {}, `Authorize ${provider} in your browser, then this finishes automatically.`),
        el('div', { class: 'toolbar' },
          el('a', { href: challenge.authorize_url, target: '_blank', rel: 'noopener' }, 'open authorization page'),
          button('I authorized — continue', (e) => finish(e.target, id), 'primary'),
          button('cancel', async () => { await api(`/oauth/${provider}/${id}`, { method: 'DELETE' }); slot.replaceChildren(); }, 'danger'))),
    );
  } else {
    // Device flow: show the code, open the verification page, poll until done.
    slot.replaceChildren(
      el('div', { class: 'card' },
        el('div', {}, 'Enter this code on GitHub:'),
        el('div', { class: 'secret' }, challenge.user_code),
        el('div', { class: 'toolbar' },
          el('a', { href: challenge.verification_uri, target: '_blank', rel: 'noopener' }, 'open verification page'),
          button('cancel', async () => { await api(`/oauth/${provider}/${id}`, { method: 'DELETE' }); slot.replaceChildren(); }, 'danger'))),
    );
  }

  async function finish(btn, loginId) {
    btn.disabled = true;
    btn.textContent = 'waiting for provider…';
    try {
      const out = await api(`/oauth/${provider}/complete`, { method: 'POST', body: { id: loginId } });
      slot.replaceChildren(
        el('div', { class: 'card ok' }, `connected ${out.provider}${out.name ? ` as ${out.name}` : ''}`),
        out.warning ? el('div', { class: 'error' }, out.warning) : null,
      );
      await onDone();
    } catch (err) {
      slot.replaceChildren(el('div', { class: 'error' }, `${provider}: ${err.message}`));
    }
  }

  // Device flows complete on their own: the server polls the provider.
  if (challenge.kind === 'device_code') finish(el('button'), id);
}

const builtins = {
  async connections(panel) {
    const loginSlot = el('div');
    const refresh = () => render(panel.parentElement, current);
    const form = el('div', { class: 'toolbar' },
      input('provider e.g. openai', 'openai'), input('auth_type', 'api_key'),
      input('name'), input('api_key / access_token'),
      button('add', async (e) => {
        const [provider, authType, name, token] = e.target.parentElement.querySelectorAll('input');
        const credentials = authType.value === 'api_key' ? { api_key: token.value } : { access_token: token.value };
        try { await api('/connections', { method: 'POST', body: { provider: provider.value, auth_type: authType.value, name: name.value, credentials } }); }
        catch (err) { alert(err.message); }
        refresh();
      }, 'primary'));
    const connect = el('div', { class: 'toolbar' },
      el('span', { class: 'muted' }, 'sign in:'),
      OAUTH_PROVIDERS.map((p) => button(p.label, () => startLogin(p.id, loginSlot, refresh))));
    // The table helper replaces the panel's children, so the toolbar is rendered into
    // its own toolbar slot and prepended afterwards (see the prepend below).
    await table(panel, [
      { title: 'provider', key: 'provider' },
      { title: 'name', key: 'name' },
      { title: 'auth', key: 'auth_type' },
      { title: 'priority', key: 'priority' },
      { title: 'tokens', render: (r) => [r.has_access_token && 'access', r.has_refresh_token && 'refresh', r.has_api_key && 'key'].filter(Boolean).join(' ') || '—', },
      { title: 'state', render: (r) => (r.is_active ? 'active' : `off (${r.backoff_level})`) },
      { title: '', render: (r) => el('span', {},
          button('test', async () => { const out = await api(`/connections/${r.id}/test`, { method: 'POST' }); alert(JSON.stringify(out)); }),
          button('delete', async () => { await api(`/connections/${r.id}`, { method: 'DELETE' }); refresh(); }, 'danger')) },
    ], async () => (await api('/connections')).data ?? []);
    panel.prepend(el('div', { class: 'toolbar' }, form), connect, loginSlot);
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
