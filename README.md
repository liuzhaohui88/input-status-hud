# AI Status Watcher

监控 [AI.INPUT.IM](https://status.input.im/) 中转站服务状态的桌面应用。服务异常或恢复时通过系统通知提醒，支持自定义监听模型与轮询策略。

基于 **Tauri 2**（Rust + WebView）构建，macOS 与 Windows 双平台。

## 功能

- **实时状态监控**：轮询 status.input.im 接口，展示各模型在线状态、可用率、延迟与最近检测时间
- **智能故障判定**：连续 N 次失败（默认 2 次）才判定故障，避免单次抖动误报
- **异常与恢复提醒**：系统通知 + 提示音（降级/恢复/监控源不可达均有独立提醒，支持冷却防止轰炸）
- **HUD 悬浮窗**：置顶小窗常驻屏幕角落，实时显示整体状态，可拖动、可隐藏
- **托盘常驻**：关闭窗口即最小化到托盘，托盘菜单支持打开面板 / 显示悬浮窗 / 立即检测 / 退出
- **灵活配置**：自定义监听模型（支持全选/全不选）、检测间隔、故障阈值、提醒冷却、开机自启，配置自动持久化

## 下载安装

从 [GitHub Releases](https://github.com/liuzhaohui88/input-status-hud/releases) 下载对应平台的安装包：

| 平台 | 安装包 |
|------|--------|
| macOS (Apple Silicon) | `AIStatusWatcher_x.x.x_aarch64.dmg` |
| Windows (x64) | `AIStatusWatcher_x.x.x_x64-setup.exe` 或 `.msi` |

首次启动会弹出主面板；之后启动默认在后台运行（仅托盘图标），点击托盘图标或菜单"打开面板"即可唤出。

## 本地开发

前置要求：Node.js 22+、pnpm 10+、Rust stable、Tauri 2 CLI 依赖（macOS 需 Xcode Command Line Tools）。

```bash
pnpm install        # 安装依赖
pnpm dev            # 启动开发模式（热更新 + Tauri）
```

## 构建打包

```bash
pnpm build          # 生成 .dmg（macOS）
```

Windows 安装包需在 Windows 环境构建，或直接使用仓库内置的 GitHub Actions：

## 持续集成

`.github/workflows/build-release.yml`：推送 `v*` tag（或在 Actions 页面手动触发）后，自动在 macOS / Windows runner 上构建并上传各自安装包到 Draft Release。

```bash
git tag v1.0.1 && git push origin v1.0.1
```

## 项目结构

```
src/          # 前端 UI（主窗口 index.html + HUD 悬浮窗 hud.html）
src-tauri/    # Rust 后端（状态轮询、通知、托盘、HUD 窗口管理、配置持久化）
  src/lib.rs  # 核心逻辑
  icons/      # 应用图标
```

## 更新图标

替换仓库根目录 `app-icon-src.png`（建议 1024×1024）后执行：

```bash
pnpm icon app-icon-src.png
```

## 许可证

[MIT](LICENSE) © liuzhaohui