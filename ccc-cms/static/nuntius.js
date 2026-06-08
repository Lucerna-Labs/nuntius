/* =====================================================================
   Nuntius admin shell — real API edition.
   ===================================================================== */

const API = {
  plugins: async () => {
    const r = await fetch('/admin/api/plugins', { credentials: 'same-origin' });
    if (!r.ok) throw new Error('plugins ' + r.status);
    return r.json();
  },
  status: async (id) => {
    const r = await fetch('/admin/api/plugins/' + encodeURIComponent(id) + '/status', { credentials: 'same-origin' });
    if (!r.ok) throw new Error('status ' + r.status);
    return r.json();
  },
  nowPlaying: async () => {
    const r = await fetch('/api/radio/nowplaying', { cache: 'no-store' });
    return r.ok ? r.json() : null;
  },
  library: async () => {
    const r = await fetch('/api/radio/library', { credentials: 'same-origin' });
    if (!r.ok) return [];
    return r.json();
  },
  setAutoDjPaused: async (paused) => {
    const path = paused ? '/admin/radio/autodj/pause' : '/admin/radio/autodj/resume';
    const r = await fetch(path, { method: 'POST', credentials: 'same-origin' });
    if (!r.ok) throw new Error('autodj ' + r.status);
    return true;
  },
};

const ICONS = {
  'icon-radio':      `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M4 11h16v9H4zM4 11l13-4M9 16h2M14 16h.01"/></svg>`,
  'icon-cms':        `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="4" y="4" width="16" height="16" rx="2"/><path d="M4 9h16M9 9v11"/></svg>`,
  'icon-blog':       `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6M9 13h6M9 17h6"/></svg>`,
  'icon-podcast':    `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="9" y="2" width="6" height="11" rx="3"/><path d="M5 10v2a7 7 0 0 0 14 0v-2M12 19v3M8 22h8"/></svg>`,
  'icon-video':      `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="2" y="6" width="14" height="12" rx="2"/><path d="m22 8-6 4 6 4z"/></svg>`,
  'icon-fed':        `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="10"/><path d="M2 12h20M12 2a15 15 0 0 1 0 20M12 2a15 15 0 0 0 0 20"/></svg>`,
  'icon-mail':       `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><rect x="2" y="4" width="20" height="16" rx="2"/><path d="m22 6-10 7L2 6"/></svg>`,
  'icon-transcript': `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><path d="M14 2v6h6M8 13h8M8 17h6M8 9h2"/></svg>`,
  'icon-seo':        `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/></svg>`,
  'icon-chart':      `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M3 3v18h18"/><path d="M7 14l4-4 4 4 5-6"/></svg>`,
  'icon-heart':      `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 1 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"/></svg>`,
  'icon-megaphone':  `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="m3 11 18-5v12L3 13zM11.6 16.8a3 3 0 1 1-5.8-1.6"/></svg>`,
  'icon-storage':    `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M21 12c0 1.66-4 3-9 3s-9-1.34-9-3M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5"/></svg>`,
  'icon-bridge':     `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M6 8a6 6 0 0 1 12 0v3M4 11h16v9H4zM9 15h6"/></svg>`,
};
const iconHtml = (k) => ICONS[k] || `<span class="mono">${(k||'?').slice(5,7).toUpperCase()}</span>`;

let plugins = [];
let activePlugin = null;

async function init() {
  try { plugins = await API.plugins(); }
  catch (e) { console.error('failed to load plugins', e); plugins = []; }
  renderRail();
  setActive('radio', { skipPanel: true });
  buildCmdkList();

  loadLibrary();
  setInterval(loadLibrary, 30_000);
  pollNowPlaying();
  setInterval(pollNowPlaying, 2_000);
}

function renderRail() {
  const list = document.getElementById('railList');
  const broadcasting = plugins.filter(p => p.primary);
  const others       = plugins.filter(p => !p.primary);

  let html = '';
  if (broadcasting.length) {
    html += `<div class="rail-section first">Broadcast</div>`;
    html += broadcasting.map(railRow).join('');
  }
  if (others.length) {
    html += `<div class="rail-section${broadcasting.length ? '' : ' first'}">Plugins</div>`;
    html += others.map(railRow).join('');
  }
  list.innerHTML = html;

  list.querySelectorAll('.rail-item').forEach(el => {
    el.addEventListener('click', () => setActive(el.dataset.id));
  });
}

function railRow(p) {
  let dotCls = '';
  if (p.state === 'live') dotCls = 'live';
  else if (p.state === 'unconfigured') dotCls = 'warn';
  else if (p.state === 'configured') dotCls = 'ok';

  let badgeHtml = '';
  if (p.badge) badgeHtml = `<span class="rail-badge">${escH(p.badge)}</span>`;
  else if (p.state === 'unconfigured') badgeHtml = `<span class="rail-badge warn">!</span>`;

  return `
    <div class="rail-item" data-id="${escH(p.id)}" title="${escH(p.name)}">
      <div class="rail-icon">
        ${iconHtml(p.icon)}
        ${dotCls ? `<span class="dot ${dotCls}"></span>` : ''}
      </div>
      <div class="rail-label">${escH(p.name)}</div>
      ${badgeHtml}
    </div>`;
}

async function setActive(id, opts = {}) {
  activePlugin = id;
  document.querySelectorAll('.rail-item').forEach(el => {
    el.classList.toggle('active', el.dataset.id === id);
  });
  if (id === 'radio') { closePanel(); return; }
  if (opts.skipPanel) return;
  const plugin = plugins.find(p => p.id === id);
  if (!plugin) return;
  let detail = {};
  try {
    const status = await API.status(id);
    detail = status.detail || {};
  } catch (e) { detail = { error: 'failed_to_load' }; }
  openPanel(plugin, detail);
}

function openPanel(plugin, detail) {
  const host = document.getElementById('panelHost');
  document.getElementById('panelIcon').innerHTML = iconHtml(plugin.icon);
  document.getElementById('panelName').textContent = plugin.name;
  document.getElementById('panelKind').textContent = plugin.kind;
  document.getElementById('panelBody').innerHTML = renderPluginView(plugin, detail);
  host.classList.add('open');
  document.getElementById('panelBody').querySelectorAll('[data-toast]').forEach(b => {
    b.addEventListener('click', () => toast(b.dataset.toast));
  });
}
function closePanel() {
  document.getElementById('panelHost').classList.remove('open');
  if (activePlugin !== 'radio') {
    activePlugin = 'radio';
    document.querySelectorAll('.rail-item').forEach(el => {
      el.classList.toggle('active', el.dataset.id === 'radio');
    });
  }
}
document.getElementById('panelClose').addEventListener('click', closePanel);

function renderPluginView(p, d) {
  if (d.error === 'failed_to_load') return renderError();
  if (d.configured === false) return renderUnconfigured(p, d);
  switch (p.id) {
    case 'cms':           return renderCms(d, p);
    case 'blog':          return renderBlog(d);
    case 'podcast':       return renderPodcast(d);
    case 'video-podcast': return renderVideoPodcast(d);
    case 'activitypub':   return renderFederation(d);
    case 'whisper':       return renderWhisper(d);
    case 'seo':           return renderSeo(d);
    case 'analytics':     return renderAnalytics(d);
    case 'bridge':        return renderBridge(d);
    case 'ads':           return renderAds(d);
    default:              return renderGeneric(p, d);
  }
}

function renderError() {
  return `<div class="status-card warn">
    <div class="status-row"><div><div class="status-label">Plugin status</div><div class="status-value">Failed to load</div></div></div>
  </div>`;
}

function renderUnconfigured(p, d) {
  const missing = (d.missing || []).map(m => `<li class="mono" style="font-size:11px; color:var(--warn); padding:2px 0;">· ${escH(m)}</li>`).join('');
  return `
    <div class="status-card warn">
      <div class="status-row">
        <div><div class="status-label">Status</div><div class="status-value">Not configured</div></div>
        <div class="status-pill warn">Setup required</div>
      </div>
    </div>
    ${missing ? `<div class="section-head"><span><span class="num">01</span>What's missing</span></div>
    <div class="status-card"><ul style="list-style:none; padding:0;">${missing}</ul></div>` : ''}
    ${d.file ? `<div class="section-head"><span><span class="num">02</span>Config file</span></div>
    <div class="toml-box mono"><span class="com"># Edit this file to configure ${escH(p.name)}</span>
<span class="key">${escH(d.file)}</span></div>` : ''}
    <div class="action-row">
      <a class="act-btn primary" href="/admin/${escH(p.id)}">▸ Open settings</a>
    </div>`;
}

function renderCms(d, p) {
  const c = d.counts || {};
  return `
    <div class="status-card ok">
      <div class="status-row">
        <div><div class="status-label">Status</div><div class="status-value">Active · ${c.pages || 0} pages live</div></div>
        <div class="status-pill ok">Operational</div>
      </div>
    </div>
    <div class="section-head"><span><span class="num">01</span>At a glance</span></div>
    <div class="field-grid two">
      <div class="field"><div class="lbl">Drafts</div><div class="val">${c.drafts || 0}</div></div>
      <div class="field"><div class="lbl">Scheduled</div><div class="val">${c.scheduled || 0}</div></div>
      <div class="field"><div class="lbl">Pending Comments</div><div class="val" style="color:var(--warn)">${c.comments_pending || 0}</div></div>
      <div class="field"><div class="lbl">Pages Live</div><div class="val">${c.pages || 0}</div></div>
    </div>`;
}

function renderBlog(d) {
  const recent = d.recent || [];
  const rows = recent.map(p => `
    <div class="list-row">
      <div class="list-meta">
        <div class="list-title">${escH(p.title || '—')}</div>
        <div class="list-sub">${statusPill(p.status)} · ${escH(p.when || '')}</div>
      </div>
      <div class="list-stat">${(p.views || 0).toLocaleString()} views</div>
      <a class="act-btn ghost" href="/admin/blog/posts/${p.id}/edit">Open</a>
    </div>`).join('');
  const c = d.counts || {};
  return `
    <div class="status-card ok">
      <div class="status-row">
        <div><div class="status-label">Status</div><div class="status-value">${c.published || 0} published · ${c.drafts || 0} drafts</div></div>
        <div class="status-pill ok">Live</div>
      </div>
    </div>
    <div class="section-head"><span><span class="num">01</span>Recent posts</span><a class="act-btn primary" href="/admin/blog/posts/new">▸ New Post</a></div>
    <div class="list">${rows || emptyList('No posts yet')}</div>`;
}

function statusPill(s) {
  const classes = { published:'ok', draft:'cold', scheduled:'warn', producing:'warn', queued:'cold', transcoding:'warn', transcribed:'ok' };
  return `<span class="status-pill ${classes[s]||''}" style="margin-right:6px;">${escH(s||'—')}</span>`;
}

function renderPodcast(d) {
  const eps = d.episodes || [];
  const rows = eps.map(e => `
    <div class="list-row">
      <div class="list-meta">
        <div class="list-title">${escH(e.title || '—')}</div>
        <div class="list-sub">${statusPill(e.status)} · ${escH(e.duration || '—')}</div>
      </div>
      <div class="list-stat">${e.downloads || 0} dl</div>
      <a class="act-btn ghost" href="/admin/podcasts/${e.id}/edit">Open</a>
    </div>`).join('');
  return `
    <div class="status-card ok">
      <div class="status-row">
        <div><div class="status-label">Audio directory</div><div class="status-value mono">${escH(d.audio_dir || '—')}</div></div>
        <div class="status-pill ok">Configured</div>
      </div>
    </div>
    <div class="section-head"><span><span class="num">01</span>Members feed</span></div>
    <div class="status-card ${d.members_token_set ? 'ok' : 'warn'}">
      <div class="status-row">
        <div><div class="status-label">Token</div>
        <div class="status-value mono">${d.members_token_set ? '••••••••• set' : 'change-me (default)'}</div></div>
        <div class="status-pill ${d.members_token_set ? 'ok' : 'warn'}">${d.members_token_set ? 'Secured' : 'Default'}</div>
      </div>
    </div>
    <div class="section-head"><span><span class="num">02</span>Episodes</span><a class="act-btn primary" href="/admin/podcasts/new">▸ New Episode</a></div>
    <div class="list">${rows || emptyList('No episodes yet')}</div>`;
}

function renderVideoPodcast(d) {
  const eps = d.episodes || [];
  const rows = eps.map(e => `
    <div class="list-row">
      <div class="list-meta">
        <div class="list-title">${escH(e.title || '—')}</div>
        <div class="list-sub">${statusPill(e.status)}${e.status === 'transcoding' ? ` · <span class="mono" style="color:var(--warn)">${e.progress || 0}%</span>` : ''}</div>
      </div>
      <div class="list-stat">${e.downloads || 0} views</div>
      <a class="act-btn ghost" href="/admin/videos/${e.id}/edit">Open</a>
    </div>`).join('');
  return `
    <div class="status-card ok">
      <div class="status-row">
        <div><div class="status-label">FFmpeg binary</div><div class="status-value mono">${escH(d.ffmpeg || '—')}</div></div>
        <div class="status-pill ok">Detected</div>
      </div>
    </div>
    <div class="section-head"><span><span class="num">01</span>Paths</span></div>
    <div class="field-grid">
      <div class="field"><div class="lbl">Source</div><div class="val">${escH(d.source_dir || '—')}</div></div>
      <div class="field"><div class="lbl">HLS Output</div><div class="val">${escH(d.hls_dir || '—')}</div></div>
    </div>
    <div class="section-head"><span><span class="num">02</span>Episodes</span><a class="act-btn primary" href="/admin/videos/new">▸ Upload Source</a></div>
    <div class="list">${rows || emptyList('No videos yet')}</div>`;
}

function renderFederation(d) {
  const a = d.actor || {};
  const inbox = (d.inbox || []).map(i => `
    <div class="list-row">
      <div class="list-meta">
        <div class="list-title"><span class="mono" style="font-size:10px; color:var(--accent); text-transform:uppercase;">${escH(i.kind)}</span> ${escH(i.who)}</div>
        <div class="list-sub">${i.about ? 're: ' + escH(i.about) : ''}</div>
      </div>
      <div class="list-stat">${escH(i.when || '')}</div>
    </div>`).join('');
  return `
    <div class="status-card ${a.host_warning ? 'warn' : 'ok'}">
      <div class="status-row">
        <div><div class="status-label">Actor</div><div class="status-value mono">@${escH(a.username || '?')}@${escH(a.host || '?')}</div></div>
        <div class="status-pill ${a.host_warning ? 'warn' : 'ok'}">${a.host_warning ? 'Check host' : 'Federated'}</div>
      </div>
      ${a.host_warning ? `<div style="margin-top:10px; font-size:12px; color:var(--warn);">⚠ ${escH(a.host_warning)}</div>` : ''}
    </div>
    <div class="section-head"><span><span class="num">01</span>Identity</span></div>
    <div class="field-grid two">
      <div class="field"><div class="lbl">Display Name</div><div class="val">${escH(a.display_name || '—')}</div></div>
      <div class="field"><div class="lbl">Followers</div><div class="val">${(a.followers || 0).toLocaleString()}</div></div>
      <div class="field"><div class="lbl">Following</div><div class="val">${a.following || 0}</div></div>
      <div class="field"><div class="lbl">RSA Keypair</div><div class="val" style="color:var(--ok);">✓ generated</div></div>
    </div>
    ${inbox ? `<div class="section-head"><span><span class="num">02</span>Inbox</span></div><div class="list">${inbox}</div>` : ''}
    <div class="action-row">
      <a class="act-btn primary" href="/admin/federation">▸ Open settings</a>
    </div>`;
}

function renderWhisper(d) {
  const queue = d.queue || [];
  const rows = queue.map(q => `
    <div class="list-row">
      <div class="list-meta">
        <div class="list-title">${escH(q.title || '—')}</div>
        <div class="list-sub">${statusPill(q.status)} · ${escH(q.duration || '—')} · model: ${escH(q.model || '—')}</div>
      </div>
      <div class="list-stat"></div>
    </div>`).join('');
  return `
    <div class="status-card ok">
      <div class="status-row">
        <div><div class="status-label">Binary</div><div class="status-value mono">${escH(d.binary || '—')}</div></div>
        <div class="status-pill ok">Available</div>
      </div>
    </div>
    <div class="section-head"><span><span class="num">01</span>Defaults</span></div>
    <div class="field-grid two">
      <div class="field"><div class="lbl">Model</div><div class="val">${escH(d.default_model || '—')}</div></div>
      <div class="field"><div class="lbl">Language</div><div class="val">${escH(d.default_language || '—')}</div></div>
      <div class="field"><div class="lbl">Output</div><div class="val">${escH(d.output_dir || '—')}</div></div>
    </div>
    <div class="section-head"><span><span class="num">02</span>Queue</span><a class="act-btn primary" href="/admin/transcripts">▸ Open queue</a></div>
    <div class="list">${rows || emptyList('Queue empty')}</div>`;
}

function renderSeo(d) {
  return `
    <div class="status-card ${d.warning ? 'warn' : 'ok'}">
      <div class="status-row">
        <div><div class="status-label">Site URL</div><div class="status-value mono">${escH(d.site_url || '—')}</div></div>
        <div class="status-pill ${d.warning ? 'warn' : 'ok'}">${d.warning ? 'Default — change' : 'Set'}</div>
      </div>
      ${d.warning ? `<div style="margin-top:10px; font-size:12px; color:var(--warn);">⚠ ${escH(d.warning)}</div>` : ''}
    </div>
    <div class="section-head"><span><span class="num">01</span>Defaults</span></div>
    <div class="field-grid">
      <div class="field"><div class="lbl">Site Name</div><div class="val">${escH(d.site_name || '—')}</div></div>
      <div class="field"><div class="lbl">Default Description</div><div class="val">${escH(d.default_description || '—')}</div></div>
    </div>
    <div class="action-row">
      <a class="act-btn primary" href="/admin/seo">▸ Edit Defaults</a>
      <a class="act-btn ghost" href="/sitemap.xml" target="_blank">View sitemap</a>
      <a class="act-btn ghost" href="/robots.txt" target="_blank">robots.txt</a>
    </div>`;
}

function renderAnalytics(d) {
  const today = d.today || {};
  const week = d.this_week || {};
  return `
    <div class="status-card ok">
      <div class="status-row">
        <div><div class="status-label">Pageviews today</div><div class="status-value" style="font-size:24px; color:var(--accent);">${(today.pageviews || 0).toLocaleString()}</div></div>
        <div class="status-pill ok">Live</div>
      </div>
    </div>
    <div class="section-head"><span><span class="num">01</span>Today</span></div>
    <div class="field-grid two">
      <div class="field"><div class="lbl">Unique Visitors</div><div class="val">${(today.unique_visitors || 0).toLocaleString()}</div></div>
      <div class="field"><div class="lbl">Top Referrer</div><div class="val">${escH(today.top_referrer || '—')}</div></div>
    </div>
    <div class="section-head"><span><span class="num">02</span>This week</span></div>
    <div class="field-grid two">
      <div class="field"><div class="lbl">Pageviews</div><div class="val">${(week.pageviews || 0).toLocaleString()}</div></div>
      <div class="field"><div class="lbl">Listeners</div><div class="val">${(week.listeners_total || 0).toLocaleString()}</div></div>
    </div>
    <div class="action-row">
      <a class="act-btn ghost" href="/admin/analytics">Detailed view</a>
    </div>`;
}

function renderBridge(d) {
  return `
    <div class="status-card ok">
      <div class="status-row">
        <div><div class="status-label">Rendezvous</div><div class="status-value">${d.active_bridges || 0} bridges · ${d.active_sessions || 0} sessions</div></div>
        <div class="status-pill ok">Content-blind</div>
      </div>
    </div>
    <div class="section-head"><span><span class="num">01</span>Doctrine</span></div>
    <div class="field-grid two">
      <div class="field"><div class="lbl">Payload logging</div><div class="val">${d.logs_payloads ? 'on' : 'off'}</div></div>
      <div class="field"><div class="lbl">Encrypted traffic</div><div class="val">${d.content_blind ? 'required' : 'unknown'}</div></div>
      <div class="field"><div class="lbl">Bridge TTL</div><div class="val">${d.bridge_ttl_secs || 0}s</div></div>
      <div class="field"><div class="lbl">Session TTL</div><div class="val">${d.session_ttl_secs || 0}s</div></div>
    </div>
    <div class="action-row">
      <a class="act-btn primary" href="/admin/bridge">Open bridge</a>
    </div>`;
}

function renderAds(d) {
  return `
    <div class="status-card ok">
      <div class="status-row">
        <div><div class="status-label">Revenue today</div><div class="status-value" style="color:var(--ok);">${escH(d.revenue_today || '—')}</div></div>
        <div class="status-pill ok">Active</div>
      </div>
    </div>
    <div class="section-head"><span><span class="num">01</span>Performance</span></div>
    <div class="field-grid two">
      <div class="field"><div class="lbl">Active Placements</div><div class="val">${d.placements || 0}</div></div>
      <div class="field"><div class="lbl">Fill Rate</div><div class="val">${escH(d.fill_rate || '—')}</div></div>
      <div class="field"><div class="lbl">Revenue (Month)</div><div class="val" style="color:var(--ok);">${escH(d.revenue_month || '—')}</div></div>
      <div class="field"><div class="lbl">Revenue (Today)</div><div class="val" style="color:var(--ok);">${escH(d.revenue_today || '—')}</div></div>
    </div>
    <div class="action-row">
      <a class="act-btn primary" href="/admin/ads">▸ Manage placements</a>
    </div>`;
}

function renderGeneric(p, d) {
  return `
    <div class="status-card">
      <div class="status-row">
        <div><div class="status-label">Plugin</div><div class="status-value">${escH(p.name)}</div></div>
        <div class="status-pill ok">Configured</div>
      </div>
    </div>
    <div class="section-head"><span><span class="num">01</span>Detail</span></div>
    <div class="status-card mono" style="font-size:11px;">
      <pre style="white-space:pre-wrap; color:var(--text-1); margin:0;">${escH(JSON.stringify(d, null, 2))}</pre>
    </div>`;
}

function emptyList(msg) {
  return `<div style="padding:30px 20px; text-align:center; color:var(--text-3); font-family:'JetBrains Mono',monospace; font-size:11px; letter-spacing:0.15em;">— ${escH(msg).toUpperCase()} —</div>`;
}

function escH(s) {
  return String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
}

/* ============================================================
   COMMAND PALETTE
   ============================================================ */
const cmdkBg = document.getElementById('cmdkBg');
const cmdkInput = document.getElementById('cmdkInput');
const cmdkList = document.getElementById('cmdkList');
let cmdkItems = [];
let cmdkSel = 0;

function buildCmdkList() {
  cmdkItems = plugins.map(p => ({
    id: p.id, label: p.name, hint: p.kind, icon: p.icon,
    action: () => setActive(p.id)
  }));
  cmdkItems.push(
    { id: 'home', label: 'Return to Radio Console', hint: 'HOME', icon: 'icon-radio', action: () => setActive('radio') },
    { id: 'logout', label: 'Sign out', hint: 'ADMIN', icon: 'icon-cms', action: () => { window.location.href = '/admin/logout'; } },
  );
  filterCmdk('');
}

function filterCmdk(q) {
  const ql = q.toLowerCase();
  const filtered = cmdkItems.filter(i => !ql || i.label.toLowerCase().includes(ql) || i.hint.toLowerCase().includes(ql));
  cmdkSel = 0;
  cmdkList.innerHTML = filtered.map((i, idx) => `
    <div class="cmdk-item ${idx === 0 ? 'sel' : ''}" data-idx="${idx}">
      <div class="ic">${iconHtml(i.icon)}</div>
      <div class="label">${escH(i.label)}</div>
      <div class="hint">${escH(i.hint)}</div>
    </div>`).join('');
  cmdkList.querySelectorAll('.cmdk-item').forEach((el, idx) => {
    el.addEventListener('click', () => { filtered[idx].action(); closeCmdk(); });
    el.addEventListener('mouseenter', () => {
      cmdkList.querySelectorAll('.cmdk-item').forEach(x => x.classList.remove('sel'));
      el.classList.add('sel');
      cmdkSel = idx;
    });
  });
  cmdkList._filtered = filtered;
}

function openCmdk() { cmdkBg.classList.add('on'); cmdkInput.value = ''; cmdkInput.focus(); filterCmdk(''); }
function closeCmdk() { cmdkBg.classList.remove('on'); }

cmdkInput.addEventListener('input', e => filterCmdk(e.target.value));
cmdkInput.addEventListener('keydown', e => {
  const filtered = cmdkList._filtered || [];
  if (e.key === 'Escape') closeCmdk();
  else if (e.key === 'ArrowDown') { e.preventDefault(); cmdkSel = Math.min(filtered.length - 1, cmdkSel + 1); updateCmdkSel(); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); cmdkSel = Math.max(0, cmdkSel - 1); updateCmdkSel(); }
  else if (e.key === 'Enter') { if (filtered[cmdkSel]) { filtered[cmdkSel].action(); closeCmdk(); } }
});
function updateCmdkSel() {
  cmdkList.querySelectorAll('.cmdk-item').forEach((el, idx) => el.classList.toggle('sel', idx === cmdkSel));
  const sel = cmdkList.querySelectorAll('.cmdk-item')[cmdkSel];
  if (sel) sel.scrollIntoView({ block: 'nearest' });
}

document.getElementById('cmdkBtn').addEventListener('click', openCmdk);
cmdkBg.addEventListener('click', e => { if (e.target === cmdkBg) closeCmdk(); });

window.addEventListener('keydown', e => {
  if ((e.metaKey || e.ctrlKey) && e.key === 'k') { e.preventDefault(); openCmdk(); }
  if (e.key === 'Escape') {
    if (cmdkBg.classList.contains('on')) closeCmdk();
    else if (document.getElementById('panelHost').classList.contains('open')) closePanel();
  }
});

/* ============================================================
   TOAST
   ============================================================ */
let toastTimer = null;
function toast(msg) {
  const t = document.getElementById('toast');
  t.textContent = msg;
  t.classList.add('on');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove('on'), 2400);
}

/* ============================================================
   RADIO CONSOLE — wired to real /api/radio/nowplaying.
   Decks/Mic/Caller/Chat are v2 placeholders.
   ============================================================ */
const rels = {
  onairBadge: $('onairBadge'), onairText: $('onairText'),
  vListeners: $('vListeners'), vPeak: $('vPeak'), vUptime: $('vUptime'), vClock: $('vClock'),
  vNet: $('vNet'), vBitrate: $('vBitrate'),
  stationName: $('stationName'), stationMeta: $('stationMeta'), mountVal: $('mountVal'),
  npLabel: $('npLabel'), npTitle: $('npTitle'), npArtist: $('npArtist'),
  npElapsed: $('npElapsed'), npDuration: $('npDuration'), npProgress: $('npProgress'),
  npBars: $('npBars'), npArtIcon: $('npArtIcon'),
  trackList: $('trackList'), libSearch: $('libSearch'),
  scheduleList: $('scheduleList'), masterL: $('masterL'), masterR: $('masterR'),
  streamAudio: $('streamAudio'),
  icecastStatus: $('icecastStatus'),
  autoState: $('autoState'), autoDjToggle: $('autoDjToggle'), footerAutoDjToggle: $('footerAutoDjToggle'),
  chatMsgs: $('chatMsgs'),
};
function $(id) { return document.getElementById(id); }

const rstate = {
  onAir: false, started_at: 0, duration: 0, last_now_unix: 0, last_now_ts_local: 0,
  uptimeStart: null, peakListeners: 0,
  autodjPaused: false,
  library: [],
};

let library = [];

async function loadLibrary() {
  try {
    library = await API.library();
    rstate.library = library;
    renderLibrary(rels.libSearch.value || '');
  } catch (e) { /* offline ok */ }
}

function renderLibrary(filter = '') {
  const f = filter.toLowerCase().trim();
  const items = library.filter(t => !f || (t.title || '').toLowerCase().includes(f) || (t.artist || '').toLowerCase().includes(f));
  if (items.length === 0) {
    rels.trackList.innerHTML = `<div style="padding: 30px 12px; text-align: center; color: var(--text-3); font-family: 'JetBrains Mono', monospace; font-size: 11px; letter-spacing: 0.15em;">— LIBRARY EMPTY —<br><br><a href="/admin/radio/tracks" style="color: var(--accent); text-decoration: none;">Upload tracks</a></div>`;
    return;
  }
  rels.trackList.innerHTML = items.slice(0, 200).map(t => `
    <div class="track" data-id="${t.id}">
      <div class="track-icon">♪</div>
      <div class="track-meta">
        <div class="track-title">${escH(t.title || 'Untitled')}</div>
        <div class="track-artist">${escH(t.artist || 'Unknown')}</div>
      </div>
      <span class="track-dur">${fmtDur(t.duration_seconds || 0)}</span>
    </div>`).join('');
}
rels.libSearch.addEventListener('input', e => renderLibrary(e.target.value));

function fmtDur(s) {
  s = Math.max(0, Math.floor(Number(s) || 0));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
  if (h > 0) return `${h}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}`;
  return `${m}:${String(sec).padStart(2, '0')}`;
}

async function pollNowPlaying() {
  let j = null;
  try { j = await API.nowPlaying(); } catch (e) { return; }
  if (!j) return;
  applyStatus(j);
}

function applyStatus(j) {
  const np = j.now_playing;
  const live = !!j.live, relay = !!j.relay;
  const autodjPaused = !!j.autodj_paused;
  const isAiring = !!(np || live || relay);

  rstate.onAir = isAiring;
  rstate.autodjPaused = autodjPaused;
  rels.onairBadge.classList.toggle('live', isAiring);
  rels.onairText.textContent = isAiring ? 'ON AIR' : 'OFF AIR';
  updateAutoDjControls();

  // Station header
  rels.stationName.textContent = j.stream_name || 'Station';
  rels.stationMeta.textContent = `STATION · ${j.mount_path || '/listen.mp3'}`;
  rels.mountVal.textContent = j.mount_path || '/listen.mp3';

  // Listeners
  const listeners = j.listeners || 0;
  rels.vListeners.textContent = listeners;
  if (listeners > rstate.peakListeners) rstate.peakListeners = listeners;
  rels.vPeak.textContent = rstate.peakListeners;

  // Uptime — start the clock when we first see airing state.
  if (isAiring && !rstate.uptimeStart) rstate.uptimeStart = Date.now();
  if (!isAiring) rstate.uptimeStart = null;

  // Now-playing source label
  if (np) {
    rstate.started_at = np.started_at || 0;
    rstate.duration = np.duration_seconds || 0;
    rstate.last_now_unix = j.now_unix || Math.floor(Date.now() / 1000);
    rstate.last_now_ts_local = Date.now() / 1000;
    rels.npLabel.textContent = `— ON AIR · ${(np.source || 'autodj').toUpperCase()} —`;
    rels.npTitle.textContent = np.title || '—';
    rels.npArtist.textContent = np.artist || '';
    rels.npDuration.textContent = fmtDur(rstate.duration);
    rels.npBars.style.display = 'flex';
    rels.npArtIcon.style.display = 'none';
  } else {
    rstate.started_at = 0; rstate.duration = 0;
    rels.npLabel.textContent = isAiring ? '— DEAD AIR —' : '— BROADCASTING NOTHING —';
    rels.npTitle.textContent = isAiring ? 'No track playing' : 'Upload tracks to start AutoDJ';
    rels.npArtist.textContent = isAiring ? 'AutoDJ idle' : 'No active source';
    if (autodjPaused) {
      rels.npLabel.textContent = '— OFF AIR —';
      rels.npTitle.textContent = 'Station is off air';
      rels.npArtist.textContent = 'Go on air when the talk block starts';
    }
    rels.npDuration.textContent = '0:00';
    rels.npBars.style.display = 'none';
    rels.npArtIcon.style.display = 'inline';
  }

  rels.icecastStatus.textContent = isAiring ? '● STREAMING' : '● READY';
  rels.icecastStatus.style.color = isAiring ? 'var(--on-air)' : 'var(--ok)';
  if (autodjPaused) {
    rels.icecastStatus.textContent = '● OFF AIR';
    rels.icecastStatus.style.color = 'var(--warn)';
  }

  // Update rail dot for radio plugin
  const radioDot = document.querySelector('.rail-item[data-id="radio"] .rail-icon .dot');
  if (radioDot) radioDot.className = 'dot ' + (isAiring ? 'live' : 'ok');

  // Wake the audio analyser once we go on air.
  if (isAiring && !audioCtx) ensureAudio(j);
}

function updateAutoDjControls() {
  const paused = !!rstate.autodjPaused;
  const label = paused ? 'GO ON AIR' : 'GO OFF AIR';
  const state = paused ? 'OFF AIR' : 'SERVER';
  if (rels.autoState) rels.autoState.textContent = state;
  [rels.autoDjToggle, rels.footerAutoDjToggle].forEach(btn => {
    if (!btn) return;
    btn.textContent = label;
    btn.classList.toggle('is-active', paused);
    btn.title = paused ? 'Resume AutoDJ and put the station back on air' : 'Stop AutoDJ and take the station off air';
  });
}

async function toggleAutoDjAirState() {
  const nextPaused = !rstate.autodjPaused;
  [rels.autoDjToggle, rels.footerAutoDjToggle].forEach(btn => { if (btn) btn.disabled = true; });
  try {
    await API.setAutoDjPaused(nextPaused);
    rstate.autodjPaused = nextPaused;
    updateAutoDjControls();
    toast(nextPaused ? 'Station is off air' : 'Station is on air');
    await pollNowPlaying();
  } catch (e) {
    toast('Could not change air state');
  } finally {
    [rels.autoDjToggle, rels.footerAutoDjToggle].forEach(btn => { if (btn) btn.disabled = false; });
  }
}

[rels.autoDjToggle, rels.footerAutoDjToggle].forEach(btn => {
  if (btn) btn.addEventListener('click', toggleAutoDjAirState);
});

/* Web Audio analyser for master meters */
let audioCtx = null, analyserL = null, analyserR = null;
function ensureAudio(statusJson) {
  if (audioCtx) return;
  try {
    rels.streamAudio.src = 'http://' + window.location.hostname + ':' + (statusJson.radio_port || 8000) + (statusJson.mount_path || '/listen.mp3');
    rels.streamAudio.crossOrigin = 'anonymous';
    rels.streamAudio.muted = true;
    rels.streamAudio.play().catch(() => {});
    audioCtx = new (window.AudioContext || window.webkitAudioContext)();
    const src = audioCtx.createMediaElementSource(rels.streamAudio);
    const splitter = audioCtx.createChannelSplitter(2);
    analyserL = audioCtx.createAnalyser(); analyserL.fftSize = 512;
    analyserR = audioCtx.createAnalyser(); analyserR.fftSize = 512;
    src.connect(splitter);
    splitter.connect(analyserL, 0);
    splitter.connect(analyserR, 1);
  } catch (e) { /* analyser unavailable; meters stay quiet */ }
}

function tick() {
  // Elapsed bar
  if (rstate.duration > 0 && rstate.started_at > 0) {
    const drift = (Date.now() / 1000) - rstate.last_now_ts_local;
    const serverNow = rstate.last_now_unix + drift;
    const elapsed = Math.max(0, serverNow - rstate.started_at);
    const pct = Math.min(100, (elapsed / rstate.duration) * 100);
    rels.npProgress.style.width = pct + '%';
    rels.npElapsed.textContent = fmtDur(elapsed);
  } else {
    rels.npProgress.style.width = '0%';
    rels.npElapsed.textContent = '0:00';
  }

  // Master meters
  if (analyserL && analyserR && rstate.onAir) {
    const aL = new Uint8Array(analyserL.fftSize);
    const aR = new Uint8Array(analyserR.fftSize);
    analyserL.getByteTimeDomainData(aL);
    analyserR.getByteTimeDomainData(aR);
    const rms = arr => {
      let s = 0;
      for (let i = 0; i < arr.length; i++) { const v = (arr[i] - 128) / 128; s += v * v; }
      return Math.sqrt(s / arr.length);
    };
    rels.masterL.style.width = Math.min(100, rms(aL) * 250) + '%';
    rels.masterR.style.width = Math.min(100, rms(aR) * 250) + '%';
  } else {
    rels.masterL.style.width = '0%';
    rels.masterR.style.width = '0%';
  }

  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);

setInterval(() => {
  const now = new Date(), pad = n => String(n).padStart(2, '0');
  rels.vClock.textContent = `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}`;
  if (rstate.onAir && rstate.uptimeStart) {
    const s = Math.floor((Date.now() - rstate.uptimeStart) / 1000);
    rels.vUptime.textContent = `${pad(Math.floor(s / 3600))}:${pad(Math.floor((s % 3600) / 60))}:${pad(s % 60)}`;
  } else {
    rels.vUptime.textContent = '00:00:00';
  }
}, 1000);

/* Tab swap on the right column */
document.querySelectorAll('.tab').forEach(t => {
  t.addEventListener('click', () => {
    document.querySelectorAll('.tab').forEach(x => x.classList.remove('on'));
    document.querySelectorAll('.tab-panel').forEach(x => x.classList.remove('on'));
    t.classList.add('on');
    document.getElementById('panel' + t.dataset.tab.charAt(0).toUpperCase() + t.dataset.tab.slice(1)).classList.add('on');
  });
});

/* Onair badge currently has no client-side toggle (server controls air state). */
rels.onairBadge.addEventListener('click', () => toast('Air state is controlled by the server. See AutoDJ / live source / relay in Engineering.'));

/* Wake audio on first user gesture (browsers block autoplay). */
document.addEventListener('click', () => { if (rstate.onAir && !audioCtx) ensureAudio({ radio_port: 8000, mount_path: '/listen.mp3' }); }, { once: true });

/* Boot */
init();
