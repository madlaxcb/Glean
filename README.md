# Glean / 拾光

Windows 本地优先的现代化 RSS / 信息流聚合阅读器。

> **当前阶段：M0b 工程底板**（M0 UI Spike 有条件通过，路径 A）  
> 内存 SQLite + `AppCommand`/`AppEvent` + demo 数据；**尚无网络抓取**（M1）。

- 开发方案：[`docs/Glean-开发方案.md`](docs/Glean-开发方案.md)
- Spike 记录：[`docs/spike-ui.md`](docs/spike-ui.md)
- 仓库：<https://github.com/madlaxcb/Glean>

## 用 GitHub Actions 产物

1. [Actions](https://github.com/madlaxcb/Glean/actions) → **windows-spike**
2. Artifact **glean-spike-windows-x64** → Windows 运行 `glean-spike.exe`
3. 需 [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/)

## 本地

```bat
cargo test -p glean-core
cargo run -p glean-app
```

## 结构

```
crates/glean-core/   # 模型、SQLite、Command/Event、GleanService
crates/glean-app/    # egui 壳 + WebView；投影 Event
```

## 许可

MIT
