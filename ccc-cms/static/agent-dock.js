(() => {
  if (window.self !== window.top) return; // Never render inside embedded studio iframes
  if (document.getElementById("nuntius-agent-dock")) return;

  const LS_COLLAPSED = "nuntius_agent_dock_collapsed";
  const LS_BOT = "nuntius_agent_dock_bot_id";

  let activeBotId = Number(localStorage.getItem(LS_BOT) || "0") || null;
  let dockState = null;

  const root = document.createElement("aside");
  root.id = "nuntius-agent-dock";
  root.setAttribute("aria-label", "AI Command Dock");
  root.innerHTML = `
    <div class="ndock__head">
      <div class="ndock__head-title"><span class="ndock__dot"></span><span>AI Team</span></div>
      <div style="display:flex;gap:4px;align-items:center;">
        <a class="ndock__icon-btn ndock__manage" href="/admin/agents" title="Manage bots">⚙</a>
        <button class="ndock__icon-btn" id="ndock-toggle" type="button" title="Toggle AI Dock">«</button>
      </div>
    </div>
    <div class="ndock__body">
      <div class="ndock__controls">
        <select class="ndock__select" id="ndock-bot-select" aria-label="Select AI bot"></select>
        <div class="ndock__ctx" id="ndock-ctx"></div>
      </div>
      <div class="ndock__feed" id="ndock-feed"></div>
      <div class="ndock__proposals" id="ndock-proposals"></div>
      <form class="ndock__form" id="ndock-form">
        <textarea class="ndock__input" id="ndock-input" placeholder="Ask your bot (status, blogs, draft Title | Body)..."></textarea>
        <div class="ndock__form-row">
          <button class="ndock__btn ndock__btn--ghost" id="ndock-clear" type="button">Clear</button>
          <button class="ndock__btn" type="submit">Send</button>
        </div>
      </form>
    </div>
  `;

  function applyCollapsed(collapsed) {
    root.classList.toggle("is-collapsed", collapsed);
    document.body.classList.toggle("nuntius-dock-collapsed", collapsed);
    document.body.classList.toggle("nuntius-dock-open", !collapsed);
    const btn = document.getElementById("ndock-toggle");
    if (btn) btn.textContent = collapsed ? "»" : "«";
    localStorage.setItem(LS_COLLAPSED, collapsed ? "1" : "0");
  }

  function esc(s) {
    return String(s).replace(/[&<>"']/g, (c) => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"
    }[c]));
  }

  function renderState(data) {
    dockState = data;
    activeBotId = data.active_bot_id;
    if (activeBotId) localStorage.setItem(LS_BOT, String(activeBotId));

    const sel = document.getElementById("ndock-bot-select");
    sel.innerHTML = data.bots
      .map((b) => `<option value="${b.id}" ${b.id === activeBotId ? "selected" : ""}>${esc(b.name)} (${esc(b.provider)})</option>`)
      .join("");

    document.getElementById("ndock-ctx").textContent = "Tab: " + window.location.pathname;

    const feed = document.getElementById("ndock-feed");
    if (!data.messages || data.messages.length === 0) {
      feed.innerHTML = `<div class="ndock__empty">Try <code>status</code>, <code>blogs</code>, or <code>draft Title | Body</code>.</div>`;
    } else {
      feed.innerHTML = data.messages
        .map((m) => `
          <div class="ndock__msg ndock__msg--${esc(m.role)}">
            <div class="ndock__msg-meta">${esc(m.sender)} · ${esc(m.when)}</div>
            <div>${m.html}</div>
          </div>
        `)
        .join("");
      feed.scrollTop = feed.scrollHeight;
    }

    const props = document.getElementById("ndock-proposals");
    if (!data.pending_proposals || data.pending_proposals.length === 0) {
      props.innerHTML = "";
    } else {
      props.innerHTML = data.pending_proposals
        .map((p) => `
          <div class="ndock__prop">
            <div class="ndock__prop-kicker">Approve Draft</div>
            <div class="ndock__prop-title">${esc(p.title)}</div>
            <div style="color:#9aa4ad;">${esc(p.site_label)} · <code>${esc(p.slug)}</code></div>
            <div class="ndock__prop-actions">
              <button class="ndock__btn" data-approve="${p.id}" type="button">Approve</button>
              <button class="ndock__btn ndock__btn--danger" data-reject="${p.id}" type="button">Reject</button>
            </div>
          </div>
        `)
        .join("");
    }
  }

  async function loadState(botId) {
    const qs = botId ? `?bot=${encodeURIComponent(botId)}` : "";
    const resp = await fetch(`/admin/agents/api/state${qs}`, { credentials: "same-origin" });
    if (!resp.ok) return false;
    const data = await resp.json();
    renderState(data);
    return true;
  }

  async function boot() {
    document.body.appendChild(root);
    const ok = await loadState(activeBotId);
    if (!ok) {
      root.remove();
      return;
    }
    const initiallyCollapsed = localStorage.getItem(LS_COLLAPSED) === "1";
    applyCollapsed(initiallyCollapsed);

    document.getElementById("ndock-toggle").addEventListener("click", () => {
      applyCollapsed(!root.classList.contains("is-collapsed"));
    });

    document.getElementById("ndock-bot-select").addEventListener("change", (e) => {
      loadState(Number(e.target.value));
    });

    const input = document.getElementById("ndock-input");
    const form = document.getElementById("ndock-form");
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const message = input.value.trim();
      if (!message || !activeBotId) return;
      input.value = "";
      const resp = await fetch("/admin/agents/api/send", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          bot_id: activeBotId,
          message,
          current_path: window.location.pathname,
        }),
      });
      if (resp.ok) renderState(await resp.json());
    });

    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        form.requestSubmit();
      }
    });

    document.getElementById("ndock-clear").addEventListener("click", async () => {
      if (!activeBotId) return;
      const resp = await fetch("/admin/agents/api/clear", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ bot_id: activeBotId }),
      });
      if (resp.ok) renderState(await resp.json());
    });

    document.getElementById("ndock-proposals").addEventListener("click", async (e) => {
      const approveId = e.target.getAttribute("data-approve");
      const rejectId = e.target.getAttribute("data-reject");
      if (approveId) {
        const resp = await fetch(`/admin/agents/api/proposals/${approveId}/approve`, {
          method: "POST",
          credentials: "same-origin",
        });
        if (resp.ok) {
          const res = await resp.json();
          if (res.state) renderState(res.state);
          if (res.edit_url) window.location.href = res.edit_url;
        }
      } else if (rejectId) {
        const resp = await fetch(`/admin/agents/api/proposals/${rejectId}/reject`, {
          method: "POST",
          credentials: "same-origin",
        });
        if (resp.ok) {
          const res = await resp.json();
          if (res.state) renderState(res.state);
        }
      }
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
