use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_notification::NotificationExt;

const STATUS_URL: &str = "https://status.input.im/api/status";
const API_DOWN_CONFIRM: u32 = 3;
const TICK_MS: u64 = 500;
const HUD_WIDTH: f64 = 300.0;
const HUD_HEIGHT: f64 = 150.0;
const HUD_RIGHT_MARGIN: f64 = 20.0;
const HUD_BOTTOM_MARGIN: f64 = 45.0;

// ---------- 配置 ----------

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct AppConfig {
    /// 需要监听的模型；为空表示监听全部
    pub models: Vec<String>,
    /// 轮询间隔（秒）
    pub poll_secs: u64,
    /// 连续几次失败才判定故障（防误报）
    pub fail_threshold: u32,
    /// 同一事件最短重复提醒间隔（秒）
    pub cooldown_secs: u64,
    /// 是否播放提示音
    pub notify_sound: bool,
    /// 恢复时是否提醒
    pub notify_recovery: bool,
    /// 开机自启
    pub autostart: bool,
    /// 显示 HUD 悬浮窗
    pub hud_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            poll_secs: 20,
            fail_threshold: 2,
            cooldown_secs: 120,
            notify_sound: true,
            notify_recovery: true,
            autostart: false,
            hud_enabled: false,
        }
    }
}

impl AppConfig {
    fn sanitized(mut self) -> Self {
        self.poll_secs = self.poll_secs.clamp(5, 3600);
        self.fail_threshold = self.fail_threshold.clamp(1, 10);
        self.cooldown_secs = self.cooldown_secs.clamp(10, 86400);
        self
    }
}

// ---------- 状态 ----------

struct AppState {
    config: Mutex<AppConfig>,
    snapshot: Mutex<Option<StatusSnapshot>>,
    last_ok: Mutex<HashMap<String, bool>>,
    fail_streak: Mutex<HashMap<String, u32>>,
    last_notify: Mutex<HashMap<&'static str, u64>>,
    api_fail_streak: Mutex<u32>,
    api_was_down: Mutex<bool>,
    baseline_done: AtomicBool,
    check_requested: AtomicBool,
    hud_pos_set: AtomicBool,
}

#[derive(Serialize, Clone, Debug)]
pub struct StatusSnapshot {
    pub all_ok: bool,
    pub generated_at: i64,
    pub monitored: Vec<ModelStatus>,
    pub all_models: Vec<String>,
    pub source_error: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct ModelStatus {
    pub model: String,
    pub ok: bool,
    pub uptime: f64,
    pub latency_ms: Option<i64>,
    pub error: Option<String>,
    pub monitored: bool,
    pub last_ts: Option<i64>,
}

#[derive(Deserialize)]
struct RawStatus {
    generated_at: i64,
    services: Vec<RawService>,
}

#[derive(Deserialize)]
struct RawService {
    model: String,
    #[serde(rename = "uptime_pct")]
    uptime_pct: f64,
    last: Option<RawLast>,
}

#[derive(Deserialize)]
struct RawLast {
    ts: i64,
    ok: bool,
    #[serde(rename = "latency_ms")]
    latency_ms: Option<i64>,
    error: Option<String>,
}

// ---------- 工具 ----------

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn log_line(_app: &AppHandle, msg: &str) {
    use std::io::Write;
    #[cfg(target_os = "macos")]
    let dir = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join("Library/Logs/AIStatusWatcher")
    };
    #[cfg(not(target_os = "macos"))]
    let dir = _app
        .path()
        .app_config_dir()
        .map(|d| d.join("logs"))
        .unwrap_or_else(|_| PathBuf::from("."));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("tauri.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[{}] {}", now_secs(), msg);
    }
}

fn fetch_status() -> Result<RawStatus, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("AIStatusWatcher/1.0")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(STATUS_URL)
        .send()
        .map_err(|e| format!("网络错误: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<RawStatus>().map_err(|e| e.to_string())
}

fn short_error(raw: &str) -> String {
    raw.replace('\n', " ").trim().chars().take(90).collect()
}

// ---------- 状态处理 ----------

fn process_raw(app: &AppHandle, state: &Arc<AppState>, raw: RawStatus) {
    log_line(
        app,
        &format!("process_raw start, {} services", raw.services.len()),
    );
    let by_model: HashMap<String, RawService> = raw
        .services
        .into_iter()
        .map(|s| (s.model.clone(), s))
        .collect();
    let mut all_models: Vec<String> = by_model.keys().cloned().collect();
    all_models.sort();

    let cfg = state.config.lock().unwrap().clone();
    let watch_set: Vec<String> = if cfg.models.is_empty() {
        all_models.clone()
    } else {
        all_models
            .iter()
            .filter(|m| cfg.models.contains(m))
            .cloned()
            .collect()
    };

    let mut model_statuses: Vec<ModelStatus> = Vec::new();
    for model in &all_models {
        let svc = by_model.get(model).unwrap();
        let last = svc.last.as_ref();
        model_statuses.push(ModelStatus {
            model: model.clone(),
            ok: last.map(|l| l.ok).unwrap_or(false),
            uptime: svc.uptime_pct,
            latency_ms: last.and_then(|l| l.latency_ms),
            error: last.and_then(|l| l.error.clone()).map(|e| short_error(&e)),
            monitored: watch_set.contains(model),
            last_ts: last.map(|l| l.ts),
        });
    }

    // 更新连续失败计数 -> 稳定状态
    let mut stabilized: HashMap<String, bool> = HashMap::new();
    {
        let mut streak = state.fail_streak.lock().unwrap();
        for model in &watch_set {
            let ok = model_statuses
                .iter()
                .find(|m| &m.model == model)
                .map(|m| m.ok)
                .unwrap_or(false);
            if ok {
                streak.insert(model.clone(), 0);
                stabilized.insert(model.clone(), true);
            } else {
                let n = streak.get(model).copied().unwrap_or(0) + 1;
                streak.insert(model.clone(), n);
                stabilized.insert(model.clone(), n < cfg.fail_threshold);
            }
        }
    }

    let new_all_ok = !watch_set.is_empty()
        && watch_set
            .iter()
            .all(|m| stabilized.get(m).copied().unwrap_or(false));

    // 通知判定
    let mut notify_events: Vec<(&'static str, String)> = Vec::new();
    if state.baseline_done.load(Ordering::SeqCst) {
        let prev_all_ok = {
            let last_ok = state.last_ok.lock().unwrap();
            !last_ok.is_empty()
                && watch_set
                    .iter()
                    .all(|m| last_ok.get(m).copied().unwrap_or(true))
        };

        if prev_all_ok && !new_all_ok {
            let mut down: Vec<String> = Vec::new();
            for model in &watch_set {
                if !stabilized.get(model).copied().unwrap_or(false) {
                    let ms = model_statuses.iter().find(|m| &m.model == model).unwrap();
                    let err = ms.error.clone().unwrap_or_default();
                    if err.is_empty() {
                        down.push(model.clone());
                    } else {
                        down.push(format!("{model}: {err}"));
                    }
                }
            }
            notify_events.push((
                "degraded",
                format!(
                    "{}/{} 个模型不可用\n{}",
                    down.len(),
                    watch_set.len(),
                    down.join("\n")
                ),
            ));
        } else if !prev_all_ok && new_all_ok {
            if cfg.notify_recovery {
                notify_events.push(("recovered", "所有监听模型均已恢复".into()));
            }
        } else if !new_all_ok {
            let newly: Vec<String> = {
                let streak = state.fail_streak.lock().unwrap();
                watch_set
                    .iter()
                    .filter(|m| {
                        !stabilized.get(*m).copied().unwrap_or(false)
                            && streak.get(*m).copied().unwrap_or(0) == cfg.fail_threshold
                    })
                    .cloned()
                    .collect()
            };
            if !newly.is_empty() {
                notify_events.push(("degraded", format!("新增故障: {}", newly.join(", "))));
            }
        }
    } else {
        state.baseline_done.store(true, Ordering::SeqCst);
    }

    // 发通知（带冷却）
    for (kind, payload) in &notify_events {
        let mut ln = state.last_notify.lock().unwrap();
        let last = ln.get(*kind).copied().unwrap_or(0);
        if now_secs().saturating_sub(last) >= cfg.cooldown_secs {
            ln.insert(kind, now_secs());
            drop(ln);
            send_notification(app, &cfg, kind, payload);
        }
    }

    // 全量恢复时同步托盘
    if new_all_ok {
        sync_tray(app, "ok");
    } else if notify_events.iter().any(|(k, _)| *k == "degraded") {
        sync_tray(app, "degraded");
    }

    // 记录新基线
    {
        let mut last_ok = state.last_ok.lock().unwrap();
        last_ok.clear();
        for (m, ok) in &stabilized {
            last_ok.insert(m.clone(), *ok);
        }
    }

    // 快照 & 推送前端
    let snap = StatusSnapshot {
        all_ok: new_all_ok,
        generated_at: raw.generated_at,
        monitored: model_statuses
            .iter()
            .filter(|m| m.monitored)
            .cloned()
            .collect(),
        all_models,
        source_error: false,
    };
    *state.snapshot.lock().unwrap() = Some(snap.clone());
    let snap_len = state
        .snapshot
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.monitored.len())
        .unwrap_or(0);
    let snap_ok = state
        .snapshot
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.all_ok)
        .unwrap_or(false);
    log_line(app, "pre-emit");
    let emit_res = app.emit("status-changed", snap);
    log_line(app, "post-emit");
    log_line(
        app,
        &format!(
            "process_raw done, monitored={}, all_ok={}, emit={:?}",
            snap_len,
            snap_ok,
            emit_res.is_ok()
        ),
    );
}

fn notify_source(app: &AppHandle, state: &Arc<AppState>, down: bool, msg: &str) {
    let kind: &'static str = if down { "source_down" } else { "source_up" };
    let cfg = state.config.lock().unwrap().clone();
    let mut ln = state.last_notify.lock().unwrap();
    let last = ln.get(kind).copied().unwrap_or(0);
    let fire = now_secs().saturating_sub(last) >= cfg.cooldown_secs;
    if fire {
        ln.insert(kind, now_secs());
        drop(ln);
        send_notification(app, &cfg, kind, msg);
    }
    sync_tray(app, if down { "source_down" } else { "recovered" });
}

fn send_notification(app: &AppHandle, cfg: &AppConfig, kind: &str, payload: &str) {
    let title = match kind {
        "degraded" => "AI.INPUT.IM 服务降级",
        "recovered" => "AI.INPUT.IM 服务已恢复",
        "source_down" => "AI.INPUT.IM 监控源不可达",
        _ => "AI.INPUT.IM 通知",
    };
    let mut builder = app.notification().builder().title(title).body(payload);
    if cfg.notify_sound {
        builder = match kind {
            "degraded" | "source_down" => builder.sound("Sosumi"),
            _ => builder.sound("Glass"),
        };
    }
    let _ = builder.show();
}

fn sync_tray(app: &AppHandle, stat: &str) {
    let tooltip = match stat {
        "ok" => "AI 状态：全部正常",
        "degraded" => "AI 状态：服务降级",
        "source_down" => "AI 状态：监控源不可达",
        _ => "AI 状态",
    };
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

// ---------- 监控线程 ----------

fn spawn_monitor(app: AppHandle, state: Arc<AppState>) {
    thread::spawn(move || {
        let mut waited_ms: u64 = u64::MAX;
        loop {
            let req = state.check_requested.swap(false, Ordering::SeqCst);
            let poll = state.config.lock().unwrap().poll_secs;
            if req || waited_ms >= poll * 1000 {
                waited_ms = 0;
                match fetch_status() {
                    Ok(raw) => {
                        log_line(&app, &format!("fetch ok, {} services", raw.services.len()));
                        {
                            let mut s = state.api_fail_streak.lock().unwrap();
                            *s = 0;
                        }
                        let was_down = {
                            let mut d = state.api_was_down.lock().unwrap();
                            let was = *d;
                            *d = false;
                            was
                        };
                        if was_down {
                            notify_source(&app, &state, false, "监控页面已恢复连接");
                        }
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            process_raw(&app, &state, raw);
                        }));
                        if let Err(e) = result {
                            let msg = e
                                .downcast_ref::<&str>()
                                .map(|s| s.to_string())
                                .or_else(|| e.downcast_ref::<String>().cloned())
                                .unwrap_or_else(|| "unknown panic".into());
                            log_line(&app, &format!("process_raw PANIC: {msg}"));
                        }
                    }
                    Err(e) => {
                        let n = {
                            let mut s = state.api_fail_streak.lock().unwrap();
                            *s += 1;
                            *s
                        };
                        let was_down = *state.api_was_down.lock().unwrap();
                        if n >= API_DOWN_CONFIRM && !was_down {
                            *state.api_was_down.lock().unwrap() = true;
                            notify_source(
                                &app,
                                &state,
                                true,
                                &format!("监控页面不可达：{e}，稍后自动重试"),
                            );
                        }
                        // 前端展示"源异常"
                        if let Some(snap) = state.snapshot.lock().unwrap().as_mut() {
                            snap.source_error = true;
                            let s = snap.clone();
                            let _ = app.emit("status-changed", s);
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(TICK_MS));
            waited_ms += TICK_MS;
        }
    });
}

// ---------- 命令 ----------

#[tauri::command]
fn get_status(state: State<'_, Arc<AppState>>) -> Option<StatusSnapshot> {
    let snap = state.snapshot.lock().unwrap().clone();
    snap
}

#[tauri::command]
fn frontend_ready(app: AppHandle) {
    log_line(&app, "frontend_ready: 前端界面已加载");
}

#[tauri::command]
fn frontend_log(app: AppHandle, msg: String) {
    log_line(&app, &format!("[frontend] {msg}"));
}

#[tauri::command]
fn get_config(state: State<'_, Arc<AppState>>) -> AppConfig {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
fn set_config(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    cfg: AppConfig,
) -> Result<(), String> {
    let cfg = cfg.sanitized();
    {
        let auto = app.autolaunch();
        let enabled = auto.is_enabled().map_err(|e| e.to_string())?;
        if cfg.autostart && !enabled {
            auto.enable().map_err(|e| e.to_string())?;
        } else if !cfg.autostart && enabled {
            auto.disable().map_err(|e| e.to_string())?;
        }
    }
    {
        let mut cur = state.config.lock().unwrap();
        *cur = cfg.clone();
    }
    save_config(&app, &cfg)?;
    if cfg.hud_enabled {
        show_hud_window(&app, state.inner())?;
    } else {
        hide_hud_window(&app)?;
    }
    Ok(())
}

#[tauri::command]
fn get_models(state: State<'_, Arc<AppState>>) -> Vec<String> {
    state
        .snapshot
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.all_models.clone())
        .unwrap_or_default()
}

#[tauri::command]
fn check_now(state: State<'_, Arc<AppState>>) {
    state.check_requested.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn autostart_status(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn show_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[tauri::command]
fn open_page() {
    let _ = tauri_plugin_opener::open_url(STATUS_URL, None::<&str>);
}

// ---------- HUD 悬浮窗 ----------

fn hud_bottom_right_position(
    origin: tauri::PhysicalPosition<i32>,
    monitor_size: tauri::PhysicalSize<u32>,
    scale_factor: f64,
) -> tauri::LogicalPosition<f64> {
    tauri::LogicalPosition::new(
        (origin.x as f64 + monitor_size.width as f64) / scale_factor - HUD_WIDTH - HUD_RIGHT_MARGIN,
        (origin.y as f64 + monitor_size.height as f64) / scale_factor
            - HUD_HEIGHT
            - HUD_BOTTOM_MARGIN,
    )
}

fn position_hud_bottom_right(app: &AppHandle, w: &tauri::WebviewWindow) -> Result<(), String> {
    if let Some(mon) = app.primary_monitor().map_err(|e| e.to_string())? {
        let origin = mon.position();
        let monitor_scale = mon.scale_factor();
        let position = hud_bottom_right_position(*origin, *mon.size(), monitor_scale);
        w.set_position(position).map_err(|e| e.to_string())?;
        let window_scale = w.scale_factor().map_err(|e| e.to_string())?;
        log_line(
            app,
            &format!(
                "HUD positioned at {:.0},{:.0}, monitor_scale={monitor_scale}, window_scale={window_scale}, monitor_origin={},{}",
                position.x, position.y, origin.x, origin.y
            ),
        );
    }
    Ok(())
}

fn show_hud_window(app: &AppHandle, state: &Arc<AppState>) -> Result<(), String> {
    let w = app
        .get_webview_window("hud")
        .ok_or_else(|| "HUD 窗口不存在".to_string())?;
    w.set_visible_on_all_workspaces(true)
        .map_err(|e| e.to_string())?;
    w.show().map_err(|e| e.to_string())?;
    if !state.hud_pos_set.swap(true, Ordering::SeqCst) {
        position_hud_bottom_right(app, &w)?;
    }
    let visible = w.is_visible().map_err(|e| e.to_string())?;
    log_line(app, &format!("HUD show completed, visible={visible}"));
    Ok(())
}

fn hide_hud_window(app: &AppHandle) -> Result<(), String> {
    let w = app
        .get_webview_window("hud")
        .ok_or_else(|| "HUD 窗口不存在".to_string())?;
    w.hide().map_err(|e| e.to_string())?;
    log_line(app, "HUD hidden");
    Ok(())
}

#[tauri::command]
fn show_hud(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    show_hud_window(&app, state.inner())
}

#[tauri::command]
fn hide_hud(app: AppHandle) -> Result<(), String> {
    hide_hud_window(&app)
}

#[tauri::command]
fn hud_ready(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    log_line(&app, "HUD frontend ready");
    if state.config.lock().unwrap().hud_enabled {
        show_hud_window(&app, state.inner())?;
    }
    Ok(())
}

#[tauri::command]
fn hud_visible(app: AppHandle) -> bool {
    app.get_webview_window("hud")
        .map(|w| w.is_visible().unwrap_or(false))
        .unwrap_or(false)
}

// ---------- 配置持久化 ----------

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("config.json"))
}

fn load_config(app: &AppHandle) -> AppConfig {
    let path = config_path(app).ok();
    let file = path.and_then(|p| std::fs::read_to_string(p).ok());
    file.and_then(|s| serde_json::from_str::<AppConfig>(&s).ok())
        .unwrap_or_default()
        .sanitized()
}

fn save_config(app: &AppHandle, cfg: &AppConfig) -> Result<(), String> {
    let path = config_path(app)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- 主入口 ----------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_status,
            frontend_ready,
            frontend_log,
            get_config,
            set_config,
            get_models,
            check_now,
            autostart_status,
            show_window,
            open_page,
            show_hud,
            hide_hud,
            hud_ready,
            hud_visible,
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle().clone();
            let first_run = config_path(&handle).map(|p| !p.exists()).unwrap_or(true);

            let state = Arc::new(AppState {
                config: Mutex::new(load_config(&handle)),
                snapshot: Mutex::new(None),
                last_ok: Mutex::new(HashMap::new()),
                fail_streak: Mutex::new(HashMap::new()),
                last_notify: Mutex::new(HashMap::new()),
                api_fail_streak: Mutex::new(0),
                api_was_down: Mutex::new(false),
                baseline_done: AtomicBool::new(false),
                check_requested: AtomicBool::new(false),
                hud_pos_set: AtomicBool::new(false),
            });
            app.manage(state.clone());

            // 首次运行显示窗口，否则保持后台
            if first_run {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                }
            }

            // 关闭窗口 = 最小化到托盘
            if let Some(w) = app.get_webview_window("main") {
                let clone = w.clone();
                w.on_window_event(move |e| {
                    if let WindowEvent::CloseRequested { api, .. } = e {
                        api.prevent_close();
                        let _ = clone.hide();
                    }
                });
            }

            // 开机自启同步
            if state.config.lock().unwrap().autostart {
                let _ = app.autolaunch().enable();
            }

            // 托盘菜单
            let open = MenuItemBuilder::with_id("open", "打开面板").build(app)?;
            let hud_toggle = MenuItemBuilder::with_id("hud", "显示/隐藏悬浮窗").build(app)?;
            let check = MenuItemBuilder::with_id("check", "立即检测").build(app)?;
            let quit = PredefinedMenuItem::quit(app, Some("退出"))?;
            let menu = MenuBuilder::new(app)
                .items(&[&hud_toggle, &open, &check, &quit])
                .build()?;

            let mut tray = TrayIconBuilder::with_id("main-tray")
                .tooltip("AI 状态")
                .menu(&menu)
                .show_menu_on_left_click(true);
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.on_menu_event(|app, event| match event.id().as_ref() {
                "open" => {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
                "check" => {
                    let st = app.state::<Arc<AppState>>();
                    st.check_requested.store(true, Ordering::SeqCst);
                }
                "hud" => {
                    let st = app.state::<Arc<AppState>>();
                    let result = match app
                        .get_webview_window("hud")
                        .and_then(|w| w.is_visible().ok())
                        .unwrap_or(false)
                    {
                        true => hide_hud_window(app),
                        false => show_hud_window(app, st.inner()),
                    };
                    if let Err(e) = result {
                        log_line(app, &format!("HUD tray toggle failed: {e}"));
                    }
                }
                _ => {}
            })
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    let app = tray.app_handle();
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
            })
            .build(app)?;

            // 启动监控线程
            spawn_monitor(handle.clone(), state.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application")
}
#[cfg(test)]
mod tests {
    use super::{fetch_status, hud_bottom_right_position, AppConfig};

    #[test]
    fn fetch_status_parses_api() {
        let raw = fetch_status().expect("API 应可访问");
        assert!(!raw.services.is_empty());
        for s in &raw.services {
            assert!(!s.model.is_empty());
            assert!(!s.uptime_pct.is_nan());
        }
    }

    #[test]
    fn config_sanitize_clamps() {
        let cfg = AppConfig {
            models: vec![],
            poll_secs: 1,
            fail_threshold: 0,
            cooldown_secs: 0,
            notify_sound: true,
            notify_recovery: true,
            autostart: false,
            hud_enabled: false,
        }
        .sanitized();
        assert_eq!(cfg.poll_secs, 5);
        assert_eq!(cfg.fail_threshold, 1);
        assert_eq!(cfg.cooldown_secs, 10);
    }

    #[test]
    fn hud_position_includes_monitor_origin() {
        let position = hud_bottom_right_position(
            tauri::PhysicalPosition::new(827, 982),
            tauri::PhysicalSize::new(1920, 1080),
            1.0,
        );
        assert_eq!(position, tauri::LogicalPosition::new(2427.0, 1867.0));
    }

    #[test]
    fn hud_position_uses_monitor_scale() {
        let position = hud_bottom_right_position(
            tauri::PhysicalPosition::new(0, 0),
            tauri::PhysicalSize::new(3024, 1964),
            2.0,
        );
        assert_eq!(position, tauri::LogicalPosition::new(1192.0, 787.0));
    }
}
