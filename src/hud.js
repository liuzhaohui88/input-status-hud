import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

const MAX_MODELS = 5;

const $ = (id) => document.getElementById(id);
const hudWindow = getCurrentWindow();

window.addEventListener("error", (event) => {
  invoke("frontend_log", { msg: `HUD error: ${event.message || event.error}` }).catch(() => {});
});
window.addEventListener("unhandledrejection", (event) => {
  invoke("frontend_log", { msg: `HUD unhandled: ${event.reason || ""}` }).catch(() => {});
});

function render(snap) {
  const dot = $("dot");
  const title = $("title");
  const time = $("time");
  const t = snap.generated_at
    ? new Date(snap.generated_at * 1000).toLocaleTimeString("zh-CN", { hour12: false })
    : "--:--:--";

  if (snap.source_error) {
    dot.className = "dot";
    title.textContent = "监控源不可达";
  } else {
    const n = snap.monitored.length;
    const down = snap.monitored.filter((m) => !m.ok).length;
    if (snap.all_ok) {
      dot.className = "dot ok";
      title.textContent = `全部正常 · ${n} 模型`;
    } else if (down === n) {
      dot.className = "dot bad";
      title.textContent = `全部故障 · ${down}/${n}`;
    } else {
      dot.className = "dot part";
      title.textContent = `${n - down}/${n} 可用 · 故障 ${down}`;
    }
  }
  time.textContent = t;

  const list = $("models");
  const ms = snap.monitored.slice(0, MAX_MODELS);
  const rest = snap.monitored.length - ms.length;
  const html = ms
    .map((m) => {
      const cls = m.ok ? "ok" : "bad";
      const name = escapeHtml(m.model);
      return `<div class="model ${m.ok ? "" : "err"}">
        <span class="m-dot ${cls}"></span>
        <span class="m-name">${name}</span>
        <span class="m-uptime">${m.uptime.toFixed(1)}%${m.ok ? "" : " · 故障"}</span>
      </div>`;
    })
    .join("");
  const more = rest > 0 ? `<div class="muted">… 还有 ${rest} 个模型</div>` : "";
  list.innerHTML = html + more;
}

function escapeHtml(s) {
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

document.querySelector(".hud").addEventListener("mousedown", async (event) => {
  if (event.button !== 0 || event.target.closest(".close")) return;
  event.preventDefault();
  try {
    await hudWindow.startDragging();
  } catch (error) {
    invoke("frontend_log", { msg: `HUD drag failed: ${error}` }).catch(() => {});
  }
});

$("close").addEventListener("click", () => invoke("hide_hud"));

listen("status-changed", (ev) => {
  if (ev.payload) render(ev.payload);
});

invoke("get_status").then((snap) => {
  if (snap) render(snap);
}).catch(() => {});

invoke("hud_ready").catch((error) => {
  invoke("frontend_log", { msg: `HUD ready failed: ${error}` }).catch(() => {});
});
