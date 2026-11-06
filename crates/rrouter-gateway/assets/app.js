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

// ---------- model list (one cached GET /api/models per page load) ----------
let modelsPromise = null;

/** Drop the cache after sync, fetch, or a custom add/delete. */
const invalidateModels = () => { modelsPromise = null; };

const getModels = () => {
  if (!modelsPromise) {
    modelsPromise = api('/models')
      .then((body) => body.data ?? [])
      .catch((err) => { modelsPromise = null; throw err; });
  }
  return modelsPromise;
};

// The nine fixed capability keys; `search` is user-set, never derived.
const CAPABILITIES = [
  ['vision', 'vision'],
  ['pdf', 'pdf'],
  ['audio_input', 'audio input'],
  ['video_input', 'video input'],
  ['image_output', 'image output'],
  ['audio_output', 'audio output'],
  ['tools', 'tools'],
  ['reasoning', 'reasoning'],
  ['search', 'search'],
];
const CHIP_TEXT = {
  vision: 'vision', pdf: 'pdf', audio_input: 'audio in', video_input: 'video in',
  image_output: 'image out', audio_output: 'audio out', tools: 'tools',
  reasoning: 'reasoning', search: 'search',
};

const capabilityChips = (capabilities = {}) => {
  const trueKeys = CAPABILITIES.filter(([key]) => capabilities[key]);
  if (!trueKeys.length) return el('span', { class: 'muted' }, '—');
  return el('span', { class: 'chips' },
    trueKeys.map(([key, label]) => el('span', { class: 'chip', title: label }, CHIP_TEXT[key] ?? key)));
};

/**
 * Options for one model pick: `<optgroup label="provider">` groups, options labelled `ref`.
 * A stored value that is gone from the list stays selected and reads `<ref> (missing)`
 * instead of silently snapping to another model.
 */
function modelOptions(models, value) {
  const options = [];
  if (value && !models.some((m) => m.ref === value)) {
    options.push(el('option', { value, title: 'no longer in the model list' }, `${value} (missing)`));
  }
  const byProvider = new Map();
  for (const model of models) {
    if (!byProvider.has(model.provider)) byProvider.set(model.provider, []);
    byProvider.get(model.provider).push(model);
  }
  for (const [provider, items] of [...byProvider].sort(([a], [b]) => a.localeCompare(b))) {
    options.push(el('optgroup', { label: provider },
      items.map((m) => el('option', { value: m.ref, title: m.display ?? m.ref }, m.ref))));
  }
  return options;
}

function modelChoice(models, { value = '', label = 'model', onChange } = {}) {
  const select = el('select', { 'aria-label': label });
  if (!value) select.append(el('option', { value: '' }, 'select a model'));
  select.append(...modelOptions(models, value));
  select.value = value;
  select.onchange = () => onChange?.(select.value);
  return select;
}

/**
 * A model pick built from `GET /api/models`. Single mode returns the `<select>` itself
 * (`.value` is the ref). Multi mode returns a `.multi-select` element whose `read()`
 * yields the refs in display order.
 */
async function modelSelect({ multi = false, value = '', label = 'model', onChange } = {}) {
  let models;
  try {
    models = await getModels();
  } catch (err) {
    return el('div', { class: 'error', role: 'alert' }, `models unavailable: ${err.message}`);
  }

  if (!multi) return modelChoice(models, { value, label, onChange });

  const root = el('div', { class: 'multi-select' });
  const rows = el('div', { class: 'multi-rows' });
  const read = () => [...rows.querySelectorAll('select')].map((s) => s.value).filter(Boolean);
  const changed = () => onChange?.(read());
  const addRow = (initial, silent) => {
    const index = rows.children.length + 1;
    const select = modelChoice(models, { value: initial, label: `${label} ${index}`, onChange: changed });
    const row = el('div', { class: 'multi-row' }, select);
    const move = (offset) => {
      const sibling = offset < 0 ? row.previousElementSibling : row.nextElementSibling;
      if (!sibling) return;
      rows.insertBefore(row, offset < 0 ? sibling : sibling.nextSibling);
      changed();
    };
    row.append(
      button('remove', () => { row.remove(); changed(); }, 'danger'),
      el('button', { type: 'button', 'aria-label': `move ${label} ${index} up`, onclick: () => move(-1) }, '↑'),
      el('button', { type: 'button', 'aria-label': `move ${label} ${index} down`, onclick: () => move(1) }, '↓'));
    rows.append(row);
    if (!silent) changed();
  };

  const initial = Array.isArray(value) ? value : value ? [value] : [];
  if (initial.length) initial.forEach((v) => addRow(v, true));
  else addRow('', true);
  root.append(rows,
    el('div', { class: 'toolbar' }, el('button', { type: 'button', onclick: () => addRow('', false) }, 'add model')));
  root.read = read;
  return root;
}

const formatCount = (value) => (Number.isFinite(value) ? value.toLocaleString() : '—');

/** `/api/models/test` answers 200 for both outcomes; only `ok` decides the colour. */
function renderTestResult(node, out) {
  const latency = Number.isFinite(out.latency_ms) ? `${out.latency_ms}ms` : '—';
  if (out.ok) {
    node.className = 'test-result ok';
    node.textContent = `ok · ${out.status ?? 200} · ${latency}${out.reply ? ` · ${out.reply}` : ''}`;
  } else {
    node.className = 'test-result bad';
    node.textContent = `failed · ${out.status ?? '—'} · ${latency} · ${out.error ?? 'no error detail'}`;
  }
}

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
    const refresh = () => render(panel, current);
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
          catch (err) { connectionError.replaceChildren(errorBlock(err, () => render(panel, current))); }
        }),
        button('delete', async () => {
          try { await api(`/connections/${r.id}`, { method: 'DELETE' }); refresh(); }
          catch (err) { connectionError.replaceChildren(errorBlock(err, () => render(panel, current))); }
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
      { title: '', render: (r) => button('delete', async () => { await api(`/keys/${r.id}`, { method: 'DELETE' }); render(panel, current); }, 'danger') },
    ], async () => (await api('/keys')).data ?? [], [
      input('name'),
      button('create key', async (e) => {
        const name = e.target.parentElement.querySelector('input').value;
        const out = await api('/keys', { method: 'POST', body: { name } });
        panel.prepend(el('div', { class: 'card' }, 'store this once: ', el('span', { class: 'secret' }, out.key)));
      }, 'primary'),
    ]);
  },

  async models(panel) {
    const view = { provider: '', q: '', connection: '', target: '' };
    const status = el('div', { class: 'muted', role: 'status', 'aria-live': 'polite' });
    const setStatus = (text, bad = false) => {
      status.className = bad ? 'error' : 'muted';
      status.textContent = text;
    };

    let rows;
    let providers;
    let connections;
    try {
      [rows, providers, connections] = await Promise.all([
        getModels(),
        api('/models/providers').then((body) => body.data ?? []),
        api('/connections').then((body) => body.data ?? []),
      ]);
    } catch (err) {
      panel.replaceChildren(errorBlock(err, () => builtins.models(panel)));
      return;
    }

    const providerFilter = el('select', { 'aria-label': 'filter models by provider' },
      el('option', { value: '' }, 'all providers'),
      providers.map((p) => el('option', { value: p.id }, p.display || p.id)));
    providerFilter.value = providers.some((p) => p.id === view.provider) ? view.provider : '';

    const query = el('input', { 'aria-label': 'filter models by id', placeholder: 'filter id or ref', value: view.q });

    const connectionSelect = el('select', { 'aria-label': 'connection used to fetch models' });
    const fetchButton = button('fetch', () => {}, '');
    fetchButton.type = 'button';

    const syncButton = button('sync models.dev', () => {}, 'primary');
    syncButton.type = 'button';

    const count = el('span', { class: 'muted' });
    const body = el('tbody');
    const tableNode = el('table', {},
      el('thead', {}, el('tr', {},
        ['provider', 'model', 'context', 'source', 'capabilities', 'enabled', 'test'].map((title) => el('th', { scope: 'col' }, title)))),
      body);

    const reload = async () => {
      invalidateModels();
      try {
        [rows, providers] = await Promise.all([
          getModels(),
          api('/models/providers').then((d) => d.data ?? []),
        ]);
        paint();
        await paintTarget();
      } catch (err) {
        setStatus(`reload failed: ${err.message}`, true);
      }
    };

    const paintConnections = () => {
      const provider = providerFilter.value;
      const usable = connections.filter((c) => !provider || c.provider === provider);
      if (!usable.some((c) => c.id === view.connection)) view.connection = usable[0]?.id ?? '';
      connectionSelect.replaceChildren(...(usable.length
        ? usable.map((c) => el('option', { value: c.id }, `${c.provider} · ${c.name || c.id}`))
        : [el('option', { value: '' }, provider ? `no connection for ${provider}` : 'no connections')]));
      connectionSelect.value = view.connection;
      connectionSelect.disabled = usable.length === 0;
      fetchButton.disabled = usable.length === 0;
      fetchButton.title = usable.length
        ? "fetch the provider's own model list over HTTP"
        : provider ? `no connection exists for ${provider}` : 'no connections configured';
    };

    const modelRow = (m) => {
      const result = el('span', { class: 'test-result', role: 'status' });
      const toggle = button(m.enabled ? 'enabled' : 'disabled', async () => {
        toggle.disabled = true;
        try {
          const suffix = m.enabled ? '' : '/enable';
          await api(`/disabled/${encodeURIComponent(m.ref)}${suffix}`, { method: 'PUT' });
          m.enabled = !m.enabled;
          toggle.textContent = m.enabled ? 'enabled' : 'disabled';
          toggle.className = m.enabled ? '' : 'danger';
          toggle.setAttribute('aria-pressed', String(m.enabled));
          toggle.setAttribute('aria-label', `${m.enabled ? 'disable' : 'enable'} ${m.ref}`);
        } catch (err) {
          result.className = 'test-result bad';
          result.textContent = err.message;
        } finally {
          toggle.disabled = false;
        }
      });
      toggle.type = 'button';
      toggle.className = m.enabled ? '' : 'danger';
      toggle.setAttribute('aria-label', `${m.enabled ? 'disable' : 'enable'} ${m.ref}`);
      toggle.setAttribute('aria-pressed', String(m.enabled));

      const test = button('test', async () => {
        test.disabled = true;
        result.className = 'test-result';
        result.textContent = 'testing…';
        try {
          renderTestResult(result, await api('/models/test', { method: 'POST', body: { model: m.ref } }));
        } catch (err) {
          result.className = 'test-result bad';
          result.textContent = err.message;
        } finally {
          test.disabled = false;
        }
      });
      test.type = 'button';
      test.setAttribute('aria-label', `test ${m.ref}`);

      const remove = m.source === 'custom'
        ? (() => {
          const node = button('delete', async () => {
            node.disabled = true;
            try {
              await api(`/models/custom?provider=${encodeURIComponent(m.provider)}&id=${encodeURIComponent(m.id)}`, { method: 'DELETE' });
              setStatus(`deleted ${m.ref}`);
              await reload();
            } catch (err) {
              setStatus(`delete failed: ${err.message}`, true);
              node.disabled = false;
            }
          }, 'danger');
          node.type = 'button';
          node.setAttribute('aria-label', `delete custom model ${m.ref}`);
          return node;
        })()
        : null;

      return el('tr', { 'data-ref': m.ref },
        el('td', {}, m.provider),
        el('td', { class: 'model-ref', title: m.display ?? m.ref }, m.ref),
        el('td', {}, m.context ? formatCount(m.context) : '—'),
        el('td', {}, m.source ?? '—', m.available === false ? el('span', { class: 'muted' }, ' · unavailable') : null),
        el('td', {}, capabilityChips(m.capabilities)),
        el('td', {}, toggle),
        el('td', { class: 'test-cell' }, test, remove, result));
    };

    const paint = () => {
      const provider = providerFilter.value;
      view.provider = provider;
      const needle = view.q.trim().toLowerCase();
      const visible = rows.filter((m) =>
        (!provider || m.provider === provider) &&
        (!needle || `${m.ref} ${m.display ?? ''}`.toLowerCase().includes(needle)));
      body.replaceChildren(...(visible.length
        ? visible.map(modelRow)
        : [el('tr', {}, el('td', { class: 'muted', colspan: '7' }, 'no models match'))]));
      count.textContent = `${visible.length} of ${rows.length} models`;
      paintConnections();
    };

    providerFilter.onchange = paint;
    query.oninput = () => { view.q = query.value; paint(); };
    connectionSelect.onchange = () => { view.connection = connectionSelect.value; };

    const targetSlot = el('span');
    let target;
    const paintTarget = async () => {
      target = await modelSelect({
        value: view.target,
        label: 'test target model',
        onChange: (value) => { view.target = value; },
      });
      if (target.tagName === 'SELECT') view.target = target.value;
      targetSlot.replaceChildren(target);
    };
    await paintTarget();
    const targetResult = el('span', { class: 'test-result', role: 'status' });
    const targetButton = button('test', async () => {
      if (!target.value) return;
      targetButton.disabled = true;
      targetResult.className = 'test-result';
      targetResult.textContent = 'testing…';
      try {
        renderTestResult(targetResult, await api('/models/test', { method: 'POST', body: { model: target.value } }));
      } catch (err) {
        targetResult.className = 'test-result bad';
        targetResult.textContent = err.message;
      } finally {
        targetButton.disabled = false;
      }
    });
    targetButton.type = 'button';

    syncButton.onclick = async () => {
      syncButton.disabled = true;
      const label = syncButton.textContent;
      syncButton.textContent = 'syncing…';
      setStatus('downloading models.dev/api.json…');
      try {
        const out = await api('/models/sync', { method: 'POST' });
        const when = Number.isFinite(out.synced_at) ? new Date(out.synced_at * 1000).toLocaleString() : '—';
        setStatus(`synced ${formatCount(out.models)} models over ${formatCount(out.providers)} providers · ${when}`);
        await reload();
      } catch (err) {
        setStatus(`sync failed: ${err.message}`, true);
      } finally {
        syncButton.disabled = false;
        syncButton.textContent = label;
      }
    };

    fetchButton.onclick = async () => {
      if (!connectionSelect.value) return;
      fetchButton.disabled = true;
      setStatus('fetching the provider model list…');
      try {
        const out = await api('/models/fetch', { method: 'POST', body: { connection_id: connectionSelect.value } });
        const failed = !(Number.isFinite(out.status) && out.status >= 200 && out.status < 300);
        setStatus(`${out.provider}: HTTP ${out.status ?? '—'} · ${formatCount(out.count)} listed · ${formatCount(out.added)} added`, failed);
        await reload();
      } catch (err) {
        setStatus(`fetch failed: ${err.message}`, true);
      } finally {
        paintConnections();
      }
    };

    const formProvider = el('select', { 'aria-label': 'custom model provider' },
      providers.map((p) => el('option', { value: p.id }, p.id)));
    const customId = el('input', { 'aria-label': 'custom model id', placeholder: 'model id' });
    const customDisplay = el('input', { 'aria-label': 'custom model display name', placeholder: 'display name' });
    const customContext = el('input', { type: 'number', min: '0', 'aria-label': 'custom model context window', placeholder: 'context' });
    const customMaxOutput = el('input', { type: 'number', min: '0', 'aria-label': 'custom model max output tokens', placeholder: 'max output' });
    const capabilityInputs = CAPABILITIES.map(([key, label]) =>
      el('label', { class: 'check' },
        el('input', { type: 'checkbox', 'data-capability': key, 'aria-label': label }),
        ` ${label}`));
    const form = el('form', { class: 'card model-form', 'aria-label': 'add custom model' },
      el('div', { class: 'form-title' }, 'add custom model'),
      el('div', { class: 'form-grid' },
        el('label', {}, 'provider', formProvider),
        el('label', {}, 'model id', customId),
        el('label', {}, 'display name', customDisplay),
        el('label', {}, 'context', customContext),
        el('label', {}, 'max output', customMaxOutput)),
      el('fieldset', { class: 'capabilities' },
        el('legend', {}, 'capabilities'),
        el('div', { class: 'checks' }, capabilityInputs)),
      el('div', { class: 'toolbar' }, button('add custom model', null, 'primary')));
    form.onsubmit = async (event) => {
      event.preventDefault();
      const id = customId.value.trim();
      if (!id) {
        setStatus('custom model id is required', true);
        customId.focus();
        return;
      }
      const capabilities = {};
      for (const [key] of CAPABILITIES) {
        capabilities[key] = Boolean(form.querySelector(`input[data-capability="${key}"]`).checked);
      }
      const payload = { provider: formProvider.value, id, capabilities };
      if (customDisplay.value.trim()) payload.display = customDisplay.value.trim();
      if (customContext.value) payload.context = Number(customContext.value);
      if (customMaxOutput.value) payload.max_output = Number(customMaxOutput.value);
      try {
        const stored = await api('/models/custom', { method: 'POST', body: payload });
        setStatus(`added ${stored.ref ?? `${payload.provider}/${id}`}`);
        form.reset();
        await reload();
      } catch (err) {
        setStatus(`add failed: ${err.message}`, true);
      }
    };

    panel.replaceChildren(
      el('div', { class: 'toolbar' }, providerFilter, query, syncButton),
      el('div', { class: 'toolbar' }, connectionSelect, fetchButton,
        el('span', { class: 'muted' }, 'test target:'), targetSlot, targetButton, targetResult),
      el('div', { class: 'toolbar' }, count, status),
      tableNode,
      form,
    );
    paint();
  },

  async aliases(panel) {
    const note = el('div', { class: 'error', role: 'status', 'aria-live': 'polite' });
    const editor = el('div');
    const aliasInput = input('alias', '');
    aliasInput.setAttribute('aria-label', 'alias name');
    const target = await modelSelect({ value: '', label: 'alias target model' });
    const save = async () => {
      const alias = aliasInput.value.trim();
      if (!alias || !target.value) {
        note.textContent = 'alias and target are required';
        return;
      }
      try {
        await api('/aliases', { method: 'POST', body: { alias, target: target.value } });
        note.textContent = '';
        render(panel, current);
      } catch (err) {
        note.replaceChildren(errorBlock(err, save));
      }
    };
    const openEditor = async (row) => {
      const select = await modelSelect({ value: row.target, label: `target for alias ${row.alias}` });
      const feedback = el('div', { class: 'error' });
      const saveEdit = async () => {
        if (!select.value) {
          feedback.textContent = 'a target is required';
          return;
        }
        try {
          await api('/aliases', { method: 'PUT', body: { alias: row.alias, target: select.value } });
          render(panel, current);
        } catch (err) {
          feedback.replaceChildren(errorBlock(err, saveEdit));
        }
      };
      editor.replaceChildren(el('div', { class: 'card toolbar' },
        el('span', {}, `edit ${row.alias}`),
        select,
        button('save', saveEdit, 'primary'),
        button('cancel', () => editor.replaceChildren()),
        feedback));
    };
    await table(panel, [
      { title: 'alias', key: 'alias' },
      { title: 'target', key: 'target' },
      { title: '', render: (r) => el('span', {},
        button('edit', () => openEditor(r)),
        button('delete', async () => { await api(`/aliases?alias=${encodeURIComponent(r.alias)}`, { method: 'DELETE' }); render(panel, current); }, 'danger')) },
    ], async () => (await api('/aliases')).data ?? [], [
      aliasInput, target, button('save', save, 'primary'), note,
    ]);
    panel.append(editor);
  },

  async combos(panel) {
    const note = el('div', { class: 'error', role: 'status', 'aria-live': 'polite' });
    const nameInput = input('name', '');
    nameInput.setAttribute('aria-label', 'combo name');
    const models = await modelSelect({ multi: true, value: [], label: 'combo model' });
    const save = async () => {
      const name = nameInput.value.trim();
      const picked = models.read ? models.read() : [];
      if (!name || !picked.length) {
        note.textContent = 'a name and at least one model are required';
        return;
      }
      try {
        await api('/combos', { method: 'POST', body: { name, models: picked } });
        render(panel, current);
      } catch (err) {
        note.replaceChildren(errorBlock(err, save));
      }
    };
    await table(panel, [
      { title: 'name', key: 'name' },
      { title: 'models', render: (r) => r.models.join(' → ') },
      { title: '', render: (r) => button('delete', async () => { await api(`/combos?name=${encodeURIComponent(r.name)}`, { method: 'DELETE' }); render(panel, current); }, 'danger') },
    ], async () => (await api('/combos')).data ?? [], [
      nameInput, models, button('save', save, 'primary'), note,
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

const panelFromHash = () => {
  if (!location.hash.startsWith('#/')) return '';
  try { return decodeURIComponent(location.hash.slice(2)); } catch { return ''; }
};

function setMenuOpen(open) {
  const shell = $('.shell');
  const menu = $('#menu-button');
  shell.classList.toggle('menu-open', open);
  menu.setAttribute('aria-expanded', String(open));
  menu.setAttribute('aria-label', open ? 'Close navigation' : 'Open navigation');
}

function navigate(id) {
  const next = `#/${encodeURIComponent(id)}`;
  if (location.hash === next) render($('#panel'), id);
  else location.hash = next;
  setMenuOpen(false);
}

function render(panel, id) {
  const def = panels.find((p) => p.id === id);
  if (!def) return;
  current = id;
  $('#panel-title').textContent = def.title ?? id;
  for (const tab of document.querySelectorAll('.tab')) {
    if (tab.dataset.id === id) tab.setAttribute('aria-current', 'page');
    else tab.removeAttribute('aria-current');
  }
  panel.replaceChildren(el('div', { class: 'muted' }, 'loading…'));
  if (def.kind === 'extension') return renderExtension(panel, def);
  return (builtins[def.id] || builtins.status)(panel);
}

function renderExtension(panel, def) {
  const frame = el('iframe', { class: 'ext', sandbox: 'allow-scripts allow-forms', src: def.url });
  panel.replaceChildren(frame);
  const token = `${def.id}:${Date.now()}`;
  frame.onload = () => frame.contentWindow.postMessage({ type: 'rrouter:panel-token', token, panel: def.id }, new URL(def.url, location.origin).origin);
}

function navButton(def) {
  return el('button', {
    class: 'tab nav-item',
    type: 'button',
    'data-id': def.id,
    'aria-label': def.title,
    onclick: () => navigate(def.id),
  },
  el('span', { class: 'nav-icon', 'aria-hidden': 'true' }, def.icon ?? ''),
  el('span', { class: 'nav-label' }, def.title));
}

async function boot() {
  const [registry, settings] = await Promise.all([api('/ui/panels'), api('/settings')]);
  const brand = registry.brand ?? settings ?? {};
  if (brand.brand_name || brand.logo_url) {
    const brandNode = $('#brand');
    const brandName = el('span', { class: 'brand-name' }, brand.brand_name || 'RRouter');
    if (brand.logo_url) brandNode.replaceChildren(el('img', { src: brand.logo_url, height: '20', alt: '' }), brandName);
    else brandNode.replaceChildren(brandName);
  }
  if (brand.brand_name) document.title = brand.brand_name;
  if (brand.brand_color) document.documentElement.style.setProperty('--accent', brand.brand_color);
  if (brand.footer_text) $('#footer').textContent = brand.footer_text;

  panels = registry.data ?? [];
  const tabs = $('#tabs');
  const panel = $('#panel');
  const builtinPanels = panels.filter((p) => p.kind !== 'extension');
  const extensionPanels = panels.filter((p) => p.kind === 'extension');
  const separator = el('div', { class: 'nav-separator', role: 'separator', 'aria-hidden': 'true' });
  separator.hidden = extensionPanels.length === 0;
  tabs.replaceChildren(...builtinPanels.map(navButton), separator, ...extensionPanels.map(navButton));

  const openPanel = panelFromHash();
  const initial = panels.some((p) => p.id === openPanel) ? openPanel : panels[0]?.id;
  if (initial) {
    if (!openPanel) history.replaceState(null, '', `#/${encodeURIComponent(initial)}`);
    render(panel, initial);
  }
  window.addEventListener('hashchange', () => {
    const id = panelFromHash();
    if (panels.some((p) => p.id === id)) render(panel, id);
  });

  $('#menu-button').onclick = () => setMenuOpen(!$('.shell').classList.contains('menu-open'));
  $('#sidebar-scrim').onclick = () => setMenuOpen(false);
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') setMenuOpen(false);
  });

  const health = $('#health');
  const healthLabel = $('#health-label');
  const beat = async () => {
    try {
      const h = await (await fetch('/health')).json();
      healthLabel.textContent = `ok ${h.uptime_secs}s`;
      health.className = 'health-status ok';
    } catch {
      healthLabel.textContent = 'down';
      health.className = 'health-status bad';
    }
  };
  beat();
  setInterval(beat, 5000);
}

boot().catch((e) => {
  $('#panel').textContent = `dashboard error: ${e.message}`;
});
