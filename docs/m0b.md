# M0b — 工程底板

> 状态：**已落地（核心）** · 2026-07-28

## 验收对照（开发方案 §9 M0b）

| 项 | 状态 |
|----|------|
| workspace：`glean-core` + `glean-app` | Done |
| `AppCommand` / `AppEvent` | Done |
| SQLite 迁移 + 空库/demo | Done（内存 + `open_path`） |
| 基础 CI：fmt + test | Done（`cargo test -p glean-core`） |
| 事件从 core 打到 UI 列表 | Done（Bootstrap → Nav/Entries → 列表/阅读） |

## 本阶段有意不做

- 网络 HTTP / feed-rs 抓取（**M1**）
- 持久化默认路径 `%APPDATA%` 接线（可 `Store::open_path`，UI 暂用内存）
- ammonia 消毒管道（M1 入库前）
- 真 H2 Overlay

## 模块

- `GleanService::handle(AppCommand) -> Vec<AppEvent>`
- 表：`folders` / `feeds` / `entries` / `entries_fts`（trigram 优先，失败降级）
- 搜索：FTS 空结果时 **LIKE** 保底（中文「世界」可命中「你好世界」）
