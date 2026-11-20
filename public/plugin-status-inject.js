(() => {
  const id = "codey-plugin-status";
  const styleId = id + "-style";
  const clock = new Intl.DateTimeFormat("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
  const failureText = { busy: "正在查询，请稍后刷新", timeout: "查询超时", invalid: "插件返回的状态无法识别", failed: "查询失败" };
  const node = (tag, css, text) => {
    const item = document.createElement(tag);
    if (css) item.className = css;
    if (text !== undefined) item.textContent = text;
    return item;
  };
  const action = (text, handler) => {
    const item = node("button", "", text);
    item.type = "button";
    item.addEventListener("click", handler);
    return item;
  };
  const installStyle = () => {
    if (document.getElementById(styleId)) return;
    const style = node("style");
    style.id = styleId;
    style.textContent = `
      #codey-settings-button[data-codey-rail-slot="true"][aria-controls="${id}"] { border-radius: 50%; background: color-mix(in srgb, CanvasText 9%, Canvas); }
      #${id} { color-scheme: light dark; --ps-ink: light-dark(#272321,#f1ede9); --ps-muted: light-dark(#6a625c,#b9b0aa); --ps-line: light-dark(#e8e0d8,#493e37); --ps-paper: light-dark(#fffcf9,#2e2622); --ps-hero: light-dark(#eef1e9,#343e34); --ps-hover: light-dark(#f2ece6,#473a32); --ps-ring: light-dark(#4d6b57,#b4d2be); --ps-error: light-dark(#963e2d,#ffb3a0); position: fixed; z-index: 2147483642; box-sizing: border-box; width: min(354px, calc(100vw - 16px)); max-height: calc(100vh - 16px); display: flex; flex-direction: column; padding: 12px; border: 1px solid var(--ps-line); border-radius: 16px; background: var(--ps-paper); color: var(--ps-ink); box-shadow: 0 12px 36px #24161033; font: 13px/1.4 system-ui,sans-serif; -webkit-app-region: no-drag; }
      :where(.light, [data-theme="light"]) #${id} { color-scheme: light; }
      :where(.dark, [data-theme="dark"]) #${id} { color-scheme: dark; }
      #${id}:focus { outline: none; }
      #${id} * { box-sizing: border-box; }
      #${id} .ps-header, #${id} .ps-footer { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
      #${id} .ps-header { padding: 0 2px 6px; }
      #${id} h2 { margin: 0; color: var(--ps-ink); font: 650 15px/1.3 system-ui,sans-serif; }
      #${id} h3 { margin: 3px 2px 7px; color: var(--ps-ink); font: 600 12px/1.4 system-ui,sans-serif; overflow-wrap: anywhere; }
      #${id} button { min-height: 30px; border: 0; border-radius: 8px; padding: 4px 7px; background: transparent; color: inherit; font: inherit; white-space: nowrap; cursor: pointer; }
      #${id} button:hover { background: var(--ps-hover); }
      #${id} button:focus-visible { outline: 2px solid var(--ps-ring); outline-offset: 2px; }
      #${id} button:disabled { cursor: default; opacity: .66; }
      #${id} .ps-list { min-height: 0; overflow-y: auto; }
      #${id} .ps-card + .ps-card { margin-top: 6px; padding-top: 8px; border-top: 1px solid var(--ps-line); }
      #${id} .ps-row { display: grid; align-items: start; grid-template-columns: minmax(0,1fr) minmax(0,1.05fr); gap: 8px; padding: 7px 5px; }
      #${id} .ps-row:first-of-type { padding: 9px 10px; border-radius: 10px; background: var(--ps-hero); }
      #${id} .ps-label { font-weight: 650; overflow-wrap: anywhere; }
      #${id} :is(.ps-detail,.ps-meta,.ps-feedback,.ps-refresh) { color: var(--ps-muted); font-size: 11px; overflow-wrap: anywhere; }
      #${id} .ps-meta { margin-top: 2px; }
      #${id} .ps-result { min-width: 0; text-align: end; }
      #${id} .ps-value { font-size: 16px; font-weight: 650; font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }
      #${id} .ps-row:first-of-type .ps-value { font-size: 19px; }
      #${id} .ps-warning { color: var(--ps-error); }
      #${id} .ps-note { margin: 4px 5px 8px; font-size: 11px; }
      #${id} .ps-footer { border-top: 1px solid var(--ps-line); padding: 4px 0 0 4px; margin-top: 5px; }
      @media (pointer: coarse) { #${id} button { min-height: 44px; } }
    `;
    document.head.appendChild(style);
  };

  const paint = (card) => {
    const nodes = [node("h3", "", card.name)];
    if (card.message) nodes.push(node("p", "ps-note ps-warning", card.message));
    if (card.rows) {
      for (const { label, detail, reading } of card.rows) {
        const row = node("div", "ps-row");
        const left = node("div");
        const right = node("div", "ps-result");
        left.append(node("div", "ps-label", label));
        if (detail) left.append(node("div", "ps-detail", detail));
        right.append(node("div", "ps-value", reading.state === "unavailable" ? "—" : reading.value));
        const at = reading.state === "unavailable" ? "" : clock.format(reading.observedAt);
        const meta = reading.state === "ready" ? at + " 更新"
          : reading.message + (at ? " · 上次 " + at : "");
        right.append(node("div", reading.state === "ready" ? "ps-meta" : "ps-meta ps-warning", meta));
        row.append(left, right);
        nodes.push(row);
      }
      if (!card.rows.length) nodes.push(node("div", "ps-note", "暂无状态"));
    }
    if (!card.message && !card.rows) nodes.push(node("div", "ps-note", "正在查询…"));
    card.body.replaceChildren(...nodes);
  };

  window.__codeyCreatePluginStatusPanels = ({ callBridge, openSettings, connected }) => {
    let live = null;
    let opening = false;
    const place = ({ anchor, panel }) => {
      const rect = anchor.getBoundingClientRect();
      const sideSpace = window.innerWidth - rect.right - 16;
      const sideways = sideSpace >= 236;
      panel.style.width = sideways ? `min(354px, ${sideSpace}px)` : "";
      const above = rect.top - 16;
      const below = window.innerHeight - rect.bottom - 16;
      const useAbove = above > below;
      const available = useAbove ? above : below;
      panel.style.maxHeight = !sideways && available >= 144 ? `${available}px` : "";
      const x = sideways ? rect.right + 8 : rect.right - panel.offsetWidth;
      const y = sideways ? rect.bottom - panel.offsetHeight + 18
        : useAbove ? rect.top - panel.offsetHeight - 8 : rect.bottom + 8;
      panel.style.left = `${Math.max(8, Math.min(x, window.innerWidth - panel.offsetWidth - 8))}px`;
      panel.style.top = `${Math.max(8, Math.min(y, window.innerHeight - panel.offsetHeight - 8))}px`;
    };
    const close = (focus = false) => {
      const state = live;
      if (!state) return;
      live = null;
      state.panel.remove();
      state.anchor.removeAttribute("aria-expanded");
      state.anchor.removeAttribute("aria-controls");
      state.anchor.setAttribute("aria-label", state.label);
      document.removeEventListener("pointerdown", state.outside, true);
      document.removeEventListener("keydown", state.escape, true);
      window.removeEventListener("resize", state.reposition);
      if (focus && state.anchor.isConnected) state.anchor.focus();
    };
    const discover = async () => {
      try {
        const result = await callBridge("/plugin-status/list", {}, { timeoutMs: 900 });
        if (result?.status === "ok" && Array.isArray(result.panels)) return result.panels;
      } catch {
        // The settings entry remains available while the bridge reconnects.
      }
      return null;
    };
    const refresh = async (state, known) => {
      if (live !== state || state.refresh.disabled) return;
      state.refresh.disabled = true;
      const panels = known ?? await discover();
      if (live !== state) return;
      if (!panels) {
        state.status.textContent = [...state.cards.values()].some((card) => card.rows?.length)
          ? "无法刷新 · 已保留上次读数" : "无法刷新";
        state.refresh.disabled = false;
        return;
      }
      if (!panels.length) { close(true); return; }
      state.cards = new Map(panels.map(({ id, generation, name }) => {
        const previous = state.cards.get(id);
        const card = previous?.generation === generation ? previous : {
          id, generation, name, body: node("section", "ps-card"),
        };
        paint(card);
        return [id, card];
      }));
      state.list.replaceChildren(...Array.from(state.cards.values(), (card) => card.body));
      state.status.textContent = "正在刷新…";
      place(state);
      await Promise.all(Array.from(state.cards.values(), async (card) => {
        let reply;
        try {
          reply = await callBridge("/plugin-status/read", { id: card.id, generation: card.generation }, { timeoutMs: 9_000 });
        } catch {
          reply = null;
        }
        if (live !== state) return;
        if (reply?.status === "ok" && Array.isArray(reply.panel?.rows)) {
          card.rows = reply.panel.rows;
          card.message = undefined;
        } else if (reply?.code === "inactive") {
          state.cards.delete(card.id);
          card.body.remove();
          if (!state.cards.size) { close(true); return; }
        } else {
          card.message = failureText[reply?.code] || "暂时无法查询";
          if (card.rows?.length) card.message += " · 以下为上次读数";
        }
        if (state.cards.has(card.id)) paint(card);
        place(state);
      }));
      if (live !== state) return;
      state.status.textContent = `${state.cards.size} 张状态卡片`;
      state.refresh.disabled = false;
    };
    const show = (anchor, panels) => {
      installStyle();
      const panel = node("section");
      panel.id = id;
      panel.tabIndex = -1;
      panel.setAttribute("role", "dialog");
      panel.setAttribute("aria-labelledby", id + "-title");
      const state = { anchor, panel, label: anchor.getAttribute("aria-label") || "打开 Codey 配置", cards: new Map() };
      const header = node("div", "ps-header");
      const title = node("h2", "", "插件状态");
      title.id = id + "-title";
      state.refresh = action("刷新", () => void refresh(state));
      state.refresh.className = "ps-refresh";
      header.append(title, state.refresh);
      state.list = node("div", "ps-list");
      const footer = node("div", "ps-footer");
      state.status = node("span", "ps-feedback");
      state.status.setAttribute("role", "status");
      footer.append(state.status, action("打开控制台 →", () => { close(); void openSettings(); }));
      panel.append(header, state.list, footer);
      document.body.append(panel);
      live = state;
      anchor.setAttribute("aria-controls", id);
      anchor.setAttribute("aria-expanded", "true");
      anchor.setAttribute("aria-label", "Codey 插件状态");
      state.outside = (event) => { if (!panel.contains(event.target) && !anchor.contains(event.target)) close(); };
      state.escape = (event) => {
        if (event.key !== "Escape") return;
        event.preventDefault();
        event.stopPropagation();
        close(true);
      };
      state.reposition = () => place(state);
      document.addEventListener("pointerdown", state.outside, true);
      document.addEventListener("keydown", state.escape, true);
      window.addEventListener("resize", state.reposition);
      panel.focus({ preventScroll: true });
      void refresh(state, panels);
    };
    const open = async (anchor) => {
      if (live?.anchor === anchor) { close(true); return; }
      if (opening) return;
      if (!connected()) { void openSettings(); return; }
      opening = true;
      const panels = await discover();
      opening = false;
      if (!anchor.isConnected) return;
      if (!panels?.length) { void openSettings(); return; }
      close();
      show(anchor, panels);
    };
    return { open, close };
  };
})();
