# Glean / 拾光

Windows 本地优先的现代化 RSS / 信息流聚合阅读器。

> **当前阶段：M0 UI Spike（路径 A — egui + WebView2）**  
> 尚无订阅业务。先验证壳与阅读区集成，再写 feed/store。

- 开发方案：[`docs/Glean-开发方案.md`](docs/Glean-开发方案.md)
- Spike 验收表：[`docs/spike-ui.md`](docs/spike-ui.md)
- 仓库：<https://github.com/madlaxcb/Glean>

## 用 GitHub Actions 产物（推荐）

1. 打开 [Actions](https://github.com/madlaxcb/Glean/actions) → 工作流 **windows-spike**
2. 选最新成功 run → Artifacts → **glean-spike-windows-x64**
3. 解压后在 **Windows** 运行 `glean-spike.exe`
4. 需要 [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)（Win11 通常已有）
5. 按 `docs/spike-ui.md` 打表；**CI 变绿 ≠ M0 Pass**

手动触发：Actions → windows-spike → Run workflow。

## 本地构建（Windows）

```bat
rustup default stable
cargo test --workspace
cargo run -p glean-app
cargo build -p glean-app --release
```

产物：`target\release\glean-spike.exe`

## 仓库结构

```
crates/glean-core/   # 领域类型、Command/Event、样例 HTML（无 UI 依赖）
crates/glean-app/    # M0 Spike：egui 三栏 + WebView 宿主
resources/reader/    # 阅读壳占位
.github/workflows/   # windows-spike CI
docs/                # 方案与 Spike 记录
```

## Spike 操作摘要

| 操作 | 说明 |
|------|------|
| H1 / H2 | 工具栏切换宿主模式 |
| j / k | 下/上一条样例文 |
| t | 深浅色 |
| Stress ×50 | 连续换文，观察内存 |
| 搜索框 | 测 IME（壳侧） |
| 文内链接 | 应取消 WebView 内导航并外开 |

## 许可

MIT（见各 crate；若改协议以仓库根声明为准）
