# Glean / 拾光

Windows 本地优先的现代化 RSS / 信息流聚合阅读器。

> **当前阶段：M1 订阅垂直切片**  
> HTTP + feed-rs 入库；阅读区 WebView2；路径 A（egui Hybrid）。

- 开发方案：[`docs/Glean-开发方案.md`](docs/Glean-开发方案.md)
- M0 Spike：[`docs/spike-ui.md`](docs/spike-ui.md)
- M0b：[`docs/m0b.md`](docs/m0b.md)
- M1：[`docs/m1.md`](docs/m1.md)
- 仓库：<https://github.com/madlaxcb/Glean>

## 用 GitHub Actions 产物

1. [Actions](https://github.com/madlaxcb/Glean/actions) → **windows-spike**
2. Artifact **glean-spike-windows-x64** → 运行 `glean-spike.exe`（需 WebView2）
3. 粘贴 RSS URL → **添加订阅** → 阅读

## 本地

```bat
cargo test -p glean-core
cargo run -p glean-app
```

## 结构

```
crates/glean-core/   # 模型、SQLite、HTTP/feed、Command/Event
crates/glean-app/    # egui 壳 + WebView2
```

## 许可

MIT
