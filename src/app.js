import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

window.addEventListener("error", (e) => {
  try {
    invoke("frontend_log", { msg: "JS error: " + (e.message || e.error) }).catch(() => {});
  } catch (_) {}
});
window.addEventListener("unhandledrejection", (e) => {
  try {
    invoke("frontend_log", { msg: "JS unhandled: " + (e.reason || "") }).catch(() => {});
  } catch (_) {}
});

const STATUS_PAGE = "https://status.input.im/";

const $ = (id) => document.getElementById(id);

let allModels = [];
let savedModels = []; // 上次保存时的监听集合（空 = 全部）

function badgeHtml(snap) {
  if (snap.source_error) {
    return `<span class="badge badge-src">监控页面不可达</span>`;
  }
  const n = snap.monitored.length;
  const down = snap.monitored.filter((m) => !m.ok).length;
  if (snap.all_ok) return `<span class="badge badge-ok">● 全部正常 · ${n} 个模型</span>`;
  if (down === n) return `<span class="badge badge-bad">● 全部故障 · ${n}/${n}</span>`;
  return `<span class="badge badge-part">● ${n - down}/${n} 可用 · 故障 ${down} 个</span>`;
}

function renderStatus(snap) {
  $("badge").innerHTML = badgeHtml(snap);
  const t = snap.generated_at
    ? new Date(snap.generated_at * 1000).toLocaleTimeString("zh-CN", { hour12: false })
    : "--:--:--";
  $("updated").textContent = `更新于 ${t}`;

  const list = $("model-list");
  if (!snap.monitored.length) {
    list.innerHTML = `<div class="muted">（未监听任何模型，将随检测自动纳入全部）</div>`;
    return;
  }
  list.innerHTML = snap.monitored
    .map((m) => {
      const cls = m.ok ? "ok" : "bad";
      const dt = m.latency_ms != null ? ` · ${m.latency_ms}ms` : "";
      const lastTs =
        m.last_ts != null
          ? ` · ${new Date(m.last_ts * 1000).toLocaleTimeString("zh-CN", { hour12: false })}`
          : "";
      const err = !m.ok && m.error ? `<span class="err" title="${escapeHtml(m.error)}">${escapeHtml(m.error)}</span>` : "";
      return `<div class="model-row">
        <span class="dot ${cls}"></span>
        <span class="name">${escapeHtml(m.model)}</span>
        <span class="meta">${m.ok ? "在线" : "故障"} · ${m.uptime.toFixed(2)}%${dt}${lastTs}</span>
        ${err}
      </div>`;
    })
    .join("");
}

function escapeHtml(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function renderPicker() {
  const box = $("model-picker");
  if (!allModels.length) {
    box.innerHTML = `<div class="muted">暂无模型清单（等待检测或监控页不可达）</div>`;
    return;
  }
  const checked = savedModels.length === 0 ? new Set(allModels) : new Set(savedModels);
  box.innerHTML = allModels
    .map((m) => {
      const on = checked.has(m) ? "checked" : "";
      return `<label><input type="checkbox" value="${escapeHtml(m)}" ${on} />${escapeHtml(m)}</label>`;
    })
    .join("");
}

function selectedModels() {
  const on = [...$("model-picker").querySelectorAll("input:checked")].map(
    (i) => i.value
  );
  return on.length === allModels.length ? [] : on;
}

function fillConfig(cfg) {
  $("poll").value = String(cfg.poll_secs);
  $("fail").value = String(cfg.fail_threshold);
  $("cooldown").value = String(cfg.cooldown_secs);
  $("sound").checked = cfg.notify_sound;
  $("recovery").checked = cfg.notify_recovery;
  $("autostart").checked = cfg.autostart;
  $("hud").checked = cfg.hud_enabled;
  savedModels = cfg.models || [];
}

function collectConfig() {
  return {
    models: selectedModels(),
    poll_secs: Number($("poll").value),
    fail_threshold: Number($("fail").value),
    cooldown_secs: Number($("cooldown").value),
    notify_sound: $("sound").checked,
    notify_recovery: $("recovery").checked,
    autostart: $("autostart").checked,
    hud_enabled: $("hud").checked,
  };
}

function flash(msg, isErr = false) {
  const el = $("save-msg");
  el.textContent = msg;
  el.className = "save-msg" + (isErr ? " err" : "");
  setTimeout(() => (el.textContent = ""), 2600);
}

async function init() {
  invoke("frontend_ready").catch(() => {});
  try {
    const cfg = await invoke("get_config");
    fillConfig(cfg);
  } catch (e) {
    console.error("load config failed", e);
  }
  try {
    const snap = await invoke("get_status");
    if (snap) {
      allModels = snap.all_models || [];
      renderStatus(snap);
      renderPicker();
    }
  } catch (e) {
    console.error("load status failed", e);
  }
  let firstEvent = true;
  listen("status-changed", (ev) => {
    const snap = ev.payload;
    if (firstEvent) {
      firstEvent = false;
      invoke("frontend_log", { msg: "status-changed 事件已收到，开始渲染" }).catch(() => {});
    }
    if (snap && snap.all_models && snap.all_models.length) {
      allModels = snap.all_models;
      renderPicker();
    }
    renderStatus(snap || { monitored: [], all_ok: false });
  });

  $("btn-check").addEventListener("click", () => invoke("check_now"));
  $("btn-open").addEventListener("click", () => invoke("open_page"));

  $("btn-select-all").addEventListener("click", () => {
    document.querySelectorAll("#model-picker input").forEach((i) => (i.checked = true));
  });
  $("btn-select-none").addEventListener("click", () => {
    document.querySelectorAll("#model-picker input").forEach((i) => (i.checked = false));
  });

  $("btn-save").addEventListener("click", async () => {
    const cfg = collectConfig();
    savedModels = cfg.models;
    try {
      await invoke("set_config", { cfg });
      renderPicker();
      invoke("check_now");
      flash("已保存，开始生效");
    } catch (e) {
      flash("保存失败：" + e, true);
    }
  });
}

window.addEventListener("DOMContentLoaded", init);
