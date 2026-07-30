# Glean / 拾光 — 完整开发方案

> 纯 Windows 本地优先的现代化 RSS / 信息流聚合阅读器  
> 参考产品：[RSSNext/Folo](https://github.com/RSSNext/Folo)  
> 文档版本：0.4.0 · 日期：2026-07-30  
> 修订说明：**M1 垂直切片已落地**（HTTP + feed-rs + 消毒入库 + UI 添加/刷新）；**§11.5 新增插件系统设计**（Rhai + 配置规则，覆盖站点适配/AI 翻译/内容增强/过滤）

---

## 0. 产品定位与和 Folo 的边界

### 0.1 一句话

**Glean（拾光）**：Windows 上的个人信息流工作台——把分散的 RSS/Atom/JSON Feed 聚合成安静、可检索、可离线的时间线。

### 0.2 从 Folo 学什么、不做什么

| 维度 | Folo | Glean |
|------|------|-------|
| 技术 | TypeScript monorepo，桌面偏 Electron/Web | **Rust 领域核心 + 本地 UI**，无云端账号 |
| 平台 | 多端 + Web | **仅 Windows**（Win10 1809+ / Win11） |
| 内容 | 文/图/音/视频 + 社区列表 + AI | **MVP 聚焦图文 RSS**；多媒体与 AI 后期可选 |
| 数据 | 服务端同步、社交 | **本地 SQLite + 可选导出**；隐私优先 |
| 许可参考 | AGPL-3.0 | 自有代码建议 MIT/Apache-2.0；**不复制** Folo 源码与图标 |

**核心体验对齐 Folo 的点：**

1. 订阅 + 分组（Category/Folder）  
2. 统一时间线 / 按源视图  
3. 已读、收藏、搜索、过滤  
4. 干净阅读区（少噪声）  
5. 主题与布局可调  

**明确不做（V1）：** 账号登录、社交关注、公开分享列表、云同步、服务端 AI、移动端。

### 0.3 产品目标优先级（拍板用）

本项目稀缺的是 **尽快做出能用的本地 RSS 阅读器**，不是「证明纯 Rust 能写 UI」。

| 优先级 | 目标 |
|--------|------|
| P0 | 可用的订阅 → 列表 → 安全阅读闭环 |
| P1 | 性能、快捷键、主题、分发 |
| 可选 | 纯 Rust UI 壳（egui）——**有条件采用，失败即切** |

因此 UI 技术采用 **「候选主路径 + 硬切换门」**，而不是默认锁死 egui。

### 0.4 成功标准（可验证）

| 指标 | 目标 |
|------|------|
| 冷启动到可操作主界面 | ≤ 1.5s（SSD，空库） |
| 常驻内存（约 200 订阅、空闲） | ≤ 150MB 理想 / ≤ 250MB 可接受 |
| 单次全量刷新 100 源 | ≤ 30s（并发受控，可配置） |
| 列表滚动 60fps | 虚拟列表，万级条目不卡顿 |
| 离线打开已缓存正文 | 100% 可用（有缓存条目） |
| M0 UI Spike | **有条件通过**（`docs/spike-ui.md`）；债：H2 真差异、标题栏 Partial |

---

## 1. 技术栈选型

### 1.1 决策原则

1. **本地优先、单进程、可审计**  
2. **正文是 HTML** → 阅读区必须可靠渲染 HTML（WebView2），禁止用 Canvas/egui 重写浏览器  
3. **Windows 第一** → WebView2（系统自带/可引导安装），避免自带 Chromium  
4. **能 crate 解决就不自研协议解析**  
5. **最大技术风险最先验证**（egui↔WebView 集成），业务代码不得早于 M0 通过  

### 1.2 推荐栈（候选主路径：Hybrid）

```
┌─────────────────────────────────────────────────────────┐
│  UI Shell（候选）: egui (eframe) — 侧栏/列表/设置/快捷键   │
│  Reader（必须）: WebView2 — 静态消毒 HTML / 外链 / 图片    │
├─────────────────────────────────────────────────────────┤
│  App Core (Rust): 订阅 · 抓取 · 解析 · 去重 · 搜索 · 设置   │
│  通信: AppCommand → Core → AppEvent → UI 投影更新         │
├─────────────────────────────────────────────────────────┤
│  SQLite (rusqlite) + FTS5(trigram) + 本地正文缓存          │
│  reqwest(rustls) + feed-rs + ammonia（消毒）              │
└─────────────────────────────────────────────────────────┘
```

| 层级 | 选型 | 理由 |
|------|------|------|
| 语言 | **Rust**（edition 以 toolchain 锁定为准） | 内存安全、单机分发、长期可维护 |
| UI 壳（候选） | **eframe + egui** | 工具型三栏、快捷键、迭代快；**须过 M0 Spike** |
| 阅读区（必须） | **WebView2**（经 wry 或 Tauri 宿主） | 正确渲染 feed HTML；远轻于 Electron |
| 异步 | **tokio** | 抓取/IO |
| HTTP | **reqwest**（rustls） | 稳定；少 OpenSSL 部署痛 |
| Feed 解析 | **feed-rs** | RSS/Atom/JSON Feed 统一模型 |
| HTML 清理 | **ammonia**（主）± lol_html 管道 | 入库前消毒 |
| DB | **SQLite + rusqlite**（bundled） | 零运维、单文件备份 |
| 迁移 | **refinery** 或 **rusqlite_migration** | 版本化 schema |
| 全文检索 | **FTS5 + `tokenize='trigram'`** | 中英文子串可搜；不接 jieba（见 §1.7） |
| 序列化 | **serde / serde_json** | 设置、OPML、导出 |
| 路径 | **directories** | `%APPDATA%\Glean` |
| 日志 | **tracing** | 结构化、可落盘 |
| 错误 | **thiserror / anyhow** | 库 / 应用边界 |
| 更新 / 安装包 | appcast + Inno/WiX 等 | 见第 6 节 |

### 1.3 UI 路径：对称双轨 + 硬切换

| 路径 | 形态 | 何时采用 |
|------|------|----------|
| **A. Hybrid（候选默认）** | egui 壳 + 嵌入/跟随的 WebView2 阅读区 | M0 Spike **全部通过** |
| **B. Tauri 2（可靠下限）** | 窗口即 WebView；壳 UI 用 Web（React/Svelte 等极简）+ Rust 命令 | M0 失败，**或**一开始就选择「先交付产品」 |

**两条路对称，不是「失败才丢脸地退 Tauri」：**

- 若执念是纯 Rust 壳 UI → 走 A，用一周 Spike 买信息。  
- 若执念是尽快能用 → **可直接选 B**，把时间花在 M1 垂直切片；`glean-core`（及后续 feed/store）原样复用。  
- **禁止**：Spike 未过仍堆订阅/DB/业务。

**否决：** 纯 egui 画 HTML；Electron。

| 其他备选 | 结论 |
|----------|------|
| iced / Slint / Dioxus | 不阻塞 V1；A 失败时与 Tauri 一并评估，默认优先 Tauri（验证次数更多） |

### 1.4 模块边界（逻辑分层；物理拆分见 §5）

个人软件 **无独立 HTTP 服务**。「后端」= 进程内领域层：

| 逻辑模块 | 职责 | 依赖禁令 |
|----------|------|----------|
| `core` | 领域模型、用例、**AppCommand / AppEvent** | **禁止**依赖 egui/wry/tauri UI |
| `feed` | HTTP、条件请求、解析、规范化 | 禁止 UI |
| `store` | SQLite、FTS、缓存路径、OPML | 只依赖 core 类型 |
| `app` | UI、命令发送、事件投影、阅读器宿主 | 组装以上 |

### 1.5 网络与解析

- **User-Agent**：`Glean/x.y.z (+https://...; personal-reader)`  
- **超时**：连接 ~10s / 整请求 ~30s（可配）  
- **条件请求**：`ETag` / `Last-Modified`  
- **压缩**：gzip/brotli；重定向有限次数；记录最终 URL  
- **格式**：RSS / Atom / JSON Feed（feed-rs 能力范围）  
- **失败**：单源失败不阻断批次；指数退避；源级错误进 `AppEvent` 与 UI  

### 1.6 本地存储

```
%APPDATA%\Glean\
  config.toml
  glean.db
  glean.db-wal
  cache\
    entries\<id>\     # 消毒后 HTML
    images\           # 可选本地图片缓存
  logs\app.log
```

SQLite：`WAL`、`busy_timeout`、外键、索引、`entries_fts`（trigram）。

### 1.7 中文搜索（明确默认）

| 方案 | 结论 |
|------|------|
| FTS5 `unicode61` | **不作为中文默认**：CJK 常整串成 token，搜「世界」打不中「你好世界」 |
| **FTS5 `tokenize='trigram'`** | **V1 默认**：无额外分词库；索引偏大、精度一般，个人体量够用 |
| jieba 等 | **不作为 V1**；仅当 trigram 体积/误报不可接受再评估 |

验收：对标题含「你好世界」的条目，查询「世界」必须命中。

---

## 2. 功能模块规划

### 2.1 模块总览

```
订阅管理 ──┬── 抓取调度 ── 解析/去重 ── store
           │         │
           │         └── AppEvent（Entry*/Feed*/Unread* …）
           │                    │
           ├── UI 投影（列表/角标）← 只听事件 + 按需窗口查询
           │
           ├── 阅读器宿主（单实例 WebView）← ReaderCommand / IPC
           │
           └── 设置 / OPML / 主题
```

**禁止：** UI 定时轮询全表「一直 query SQLite」作为主更新路径。  
**允许：** 收到事件后，对**当前视口**做一次窗口化 SQL（虚拟列表）。

### 2.2 订阅管理

| 功能 | 说明 | 优先级 |
|------|------|--------|
| 添加订阅 | URL → 探测 → 预览 → 确认 | P0 |
| HTML 发现 feed | `<link rel="alternate" …>` | P0 |
| OPML 导入/导出 | OPML 2.0 | P0 |
| 分组 | V1 **单级**文件夹 | P0 |
| 编辑 | 标题覆盖、间隔、静音（不进未读） | P0 |
| 删除 | 级联条目；星标策略可配 | P0 |
| 刷新 | 全局默认 + 每源覆盖；手动/定时/启动 | P0 |
| 源状态 | 上次成功、连续失败 | P1 |
| 图标 | favicon 缓存 | P1 |

```text
Folder { id, name, sort_order }
Feed   { id, folder_id?, url, site_url?, title, etag?, last_modified?,
         interval_secs, muted, error?, updated_at }
Entry  { id, feed_id, guid, url, title, author?, published_at,
         summary?, content_path?, read, starred, created_at }
```

**guid 去重：** `(feed_id, guid)` 唯一；无 guid 时用规范化 URL 或标题+日期哈希。

### 2.3 聚合与展示

| 视图 | 行为 |
|------|------|
| 全部未读 | 跨源时间倒序 |
| 星标 | 收藏 |
| 按文件夹 / 按源 | 范围过滤 |
| 今日/最近 | P1 |

虚拟滚动 + SQL 限域；muted 不计入全局未读。

### 2.4 阅读体验

| 功能 | 说明 | 优先级 |
|------|------|--------|
| 已读/未读 | 打开可自动已读；全部已读 | P0 |
| 星标 | 独立视图 | P0 |
| 搜索 | FTS trigram：标题/作者/摘要（正文视缓存策略） | P0 |
| 过滤 | 未读、星标、源、时间 | P0 |
| 外开浏览器 | 系统默认浏览器 | P0 |
| 远程图片策略 | 见 §2.4.1 | P0（默认值） |
| 字体/行宽 | CSS 变量 | P1 |
| 全文抽取 / 媒体 | 后期 | P2 |

#### 2.4.1 远程图片策略（隐私）

打开含 `<img src="https://…">` 的文章等于向源站暴露「有人读了」。设置三项，**默认 Block**：

| 模式 | 行为 |
|------|------|
| **Block remote image（默认）** | 消毒阶段去掉/占位远程图；不发起请求 |
| **Load on demand** | 占位 +「显示图片」后加载 |
| **Always allow** | 允许远程图（仍无 JS） |

本地已缓存图片走 `file`/`asset` 协议，不受 Block 影响。

### 2.5 离线

1. 抓取后写 **消毒 HTML** 到 `cache/entries/<id>/`  
2. 可选下图并改写 src（默认关）  
3. 离线模式只读库与缓存  
4. 缓存 LRU（最大 MB 可配）  

### 2.6 设置

- 外观、三栏比例、打开标记已读、刷新并发  
- **图片策略**、搜索相关  
- 代理（P1）、数据目录/备份/清缓存  
- 快捷键见 §3.4  

### 2.7 非功能

- 单实例；托盘 P1；i18n 键尽早抽出  

---

## 3. UI/UX 设计规范

### 3.1 设计语言

- **气质：** 安静、纸感；「拾光」= 收拢信息，不做成瘾信息流  
- **参考：** Win11 间距圆角 + Folo / Reeder / NetNewsWire 信息层级  
- **色：** 浅 `#F7F7F5` / `#FFFFFF`；深 `#1C1C1E` / `#2C2C2E`；未读 semibold  
- **字体：** UI Segoe UI Variable；阅读区 WebView 可切换衬线  
- **图标：** 自绘或可商用；**禁止** Folo `icons/mgc`  

### 3.2 三栏信息架构

```
┌──────────┬──────────────┬────────────────────────────┐
│ 导航     │ 条目列表      │ 阅读区 (WebView)            │
│ 全部/星标 │ 虚拟列表      │ 标题/元信息/正文            │
│ 文件夹   │ 搜索/过滤     │                            │
│ 订阅源   │              │                            │
└──────────┴──────────────┴────────────────────────────┘
```

窄屏 &lt;1280：双栏 + 源抽屉；最小约 900×600。

### 3.3 交互

- 刷新：源旁进度，非全屏阻塞  
- 空状态三态；源错误可折叠，不模态轰炸  
- 分隔条拖动：阅读区位置同步须节流（Spike 验收）  

### 3.4 键盘与可访问性

| 键 | 动作 |
|----|------|
| `j` / `k` | 下/上一条 |
| `o` / `Enter` | 原文 |
| `m` / `s` | 已读 / 星标 |
| `r` | 刷新当前范围 |
| `/` | 搜索 |
| `Esc` | 清空/关闭 |
| `Ctrl+,` / `Ctrl+N` | 设置 / 添加订阅 |

- 焦点：`l` 列表等规则写清；egui↔WebView 焦点为 Spike 项  
- 对比度约 AA；读屏 V1 如实写限制（egui 有限）  

---

## 4. 开发环境与工作流

### 4.1 环境（Windows）

1. VS 2022 Build Tools（C++）  
2. `rustup`，`x86_64-pc-windows-msvc`  
3. WebView2 Evergreen  
4. rust-analyzer；可选 nextest / packager  

```text
cargo build
cargo test
cargo run -p glean-app   # 或单包时 cargo run
```

### 4.2 质量门禁

`rustfmt` / `clippy` / 可选 `cargo deny` / `cargo audit`；`rust-toolchain.toml` 锁 stable。

**注意：** CI 全量流水线放在 **M0 Spike 通过之后** 再完善；Spike 阶段只保能编译运行。

### 4.3 Git

- `main` + `feature/*` / `fix/*`  
- Conventional Commits  
- 忽略 `target/`、`*.db`、本地配置；禁止密钥入仓  

### 4.4 构建与测试

- Debug / Release：`cargo build` / `--release`  
- 单测：解析、去重、消毒、事件归约  
- 集成：store 迁移、mock HTTP 刷新  
- **M0：** 手工量化清单（§9.0），不强制 UI 自动化  

---

## 5. 文件组织结构

### 5.1 原则：逻辑分模，物理晚拆

评审结论：**一上来四个 crate 偏早**；但 **「完全单包、零边界」不可取**——Rust 要用编译器挡住 `core` 依赖 UI。

**V1 推荐物理结构（折中）：**

| 阶段 | 结构 |
|------|------|
| **现在** | 二包：`glean-core`（纯领域 + Command/Event）+ `glean-app`（feed/store/ui/reader 用 **mod** 分层） |
| **边界稳定后**（约近万行或需单测隔离时） | 再拆 `glean-feed`、`glean-store` |

```text
Glean/
├── Cargo.toml                 # workspace: core + app
├── rust-toolchain.toml
├── docs/
│   ├── Glean-开发方案.md
│   ├── spike-ui.md            # M0 记录与截图/结论（Spike 后补）
│   ├── architecture.md
│   └── user-guide.md
├── scripts/
│   ├── build-release.ps1
│   └── package-inno.ps1
├── resources/
│   ├── icons/
│   ├── i18n/
│   └── reader/                # shell.html + CSS；默认无业务 JS
├── crates/
│   ├── glean-core/            # 领域、AppCommand、AppEvent；禁 UI
│   └── glean-app/
│       └── src/
│           ├── main.rs
│           ├── ui/            # egui 或 Tauri 前端桥
│           ├── feed/
│           ├── store/
│           └── reader/        # WebView 宿主与 IPC
├── testdata/feeds/
├── packaging/
├── target/                    # gitignore
└── dist/                      # gitignore
```

若直接选 **Tauri 路径 B**：`glean-app` 改为 Tauri 工程布局，**仍依赖 `glean-core`**；`feed`/`store` 可先在 app 内 mod。

### 5.2 分层约定

- `glean-core`：**零** egui/wry/tauri 依赖（CI 可 `cargo tree -p glean-core` 抽查）  
- `app` 内 `feed`/`store`/`ui`/`reader` 目录单向依赖：ui → core 命令；feed/store → core 类型  
- 拆 crate 时目录基本原样搬出  

### 5.3 产物

| 产物 | 路径 |
|------|------|
| exe | `target/release/glean.exe` |
| 安装包 | `dist/Glean-Setup-x.y.z.exe` |
| 便携 zip（可选） | `dist/Glean-x.y.z-portable.zip` |

---

## 6. 发布与部署

### 6.1 打包

Release 构建 → Inno Setup / WiX；检测 WebView2 Runtime；可选便携目录模式 `./data`。  
个人阶段可无签名；有证书则签，降低 SmartScreen 阻拦。

### 6.2 更新

V1：Releases 上 `appcast.json`（版本、changelog、URL、SHA256）→ 提示 + 打开下载。  
V2：校验哈希与 ed25519/minisign → 拉起安装器。不做强制差分。

### 6.3 版本

SemVer；标签 `v0.1.0`。  
DB：`user_version` / migrations；失败提示备份路径。

| 阶段 | 版本 | 范围 |
|------|------|------|
| Spike | 0.0.x | 仅 UI 宿主实验，可丢弃 |
| 骨架+闭环 | 0.1.x | M0 通过后的窗体、DB、真源阅读 |
| 可用 | 0.2.x | 已读/星标/搜索/OPML/离线 |
| 打磨 | 0.3.x | 主题、快捷键、托盘、更新 |
| 稳定 | 1.0.0 | 指标、安装包、手册 |

---

## 7. 性能与安全

### 7.1 性能

| 点 | 策略 |
|----|------|
| 启动 | 先壳后库；WebView 延迟创建；启动刷新不堵首帧 |
| 抓取 | 并发 4–8；事件通知 UI，不靠狂轮询 |
| 列表 | 视口查询 + 虚拟列表 |
| 阅读器 | **单实例复用**；`OpenEntry` 换 DOM/文档，禁止每文 new WebView |
| 磁盘 | WAL、缓存 LRU |

### 7.2 安全与隐私（M1 即生效的默认值）

| 项 | 默认 | 说明 |
|----|------|------|
| HTML 消毒 | 开 | 去 script、事件属性、危险 scheme |
| **WebView 脚本** | **`IsScriptEnabled = false`（或等价）** | 正文是静态 HTML，**不需要 JS**；安全收益极高、成本近零 |
| 导航 | 拦截 | 仅 http(s)/mailto；外链系统浏览器 |
| 远程图 | **Block** | 见 §2.4.1 |
| 路径 | 内部 id | 防缓存目录穿越 |
| 遥测 | 无 | 若有必须 opt-in |
| 更新 | 哈希（+ 签名 V2） | 防劫持 |

**说明：** 阅读壳若完全无 JS，主题/换文可通过 **重新 `set_html` / 导航 data/virtual URL** 或 WebView2 允许的极窄注入完成；**不要**为了省事默认开全局 JS。若未来必须开 JS，须单独威胁评审 + CSP，且不得执行 feed 自带脚本。

**威胁模型：** 本地单用户；主敌是恶意 feed 与供应链，不是多租户服务器。

---

## 8. 架构设计（实施级）

### 8.1 线程与更新模型

```text
UI 线程     : 绘制、输入、把 AppEvent 归约进 UiState
Tokio       : 抓取、解析、DB 写
Reader 宿主 : WebView2 实例（嵌入或跟随窗口）

UI ──AppCommand──► Core/Workers ──AppEvent──► UI 投影
                         │
                         └── ReaderCommand ──► 单实例 WebView
```

**UI 不主动轮询库作为主路径**；事件驱动刷新角标与脏区，视口数据按需查。

### 8.2 AppCommand / AppEvent（须实现并进验收）

**AppCommand（UI → Core，示例）：**

```text
AddFeed { url }
RefreshFeeds { scope }
MarkRead { ids, read }
ToggleStar { id }
Search { query }
ImportOpml { path }
SetSetting { … }
OpenEntry { id }          # 也可经 UI 直接转 Reader，但仍要记已读等副作用
```

**AppEvent（Core → UI，示例）：**

```text
FeedAdded / FeedUpdated / FeedDeleted / FeedError
EntryAdded / EntryUpdated / EntriesBatchUpserted
UnreadChanged { feed_id?, delta_or_total }
SearchReady { token }
RefreshProgress { done, total }
```

M1 验收：刷新完成后列表与未读 **仅通过事件**（+ 一次视口查询）更新，而不是 `timer` 全表扫。

### 8.3 阅读器生命周期与 IPC

```text
App/UI
  │  ReaderCommand::OpenEntry { id, title, html, theme, image_policy }
  ▼
ReaderHost（单实例）
  │  若脚本关闭：set_html(shell+content) 或 NavigateToString
  │  若将来极窄桥：IPC → reader 仅官方壳脚本（仍禁止 feed JS）
  ▼
DOM 替换 / 全文更新
  │
  ▼ 外链请求
系统浏览器（WebView 内不跟进站点 SPA）
```

- **禁止**每篇文章 `new WebView`  
- 主题切换：同一实例更新 CSS 变量或整页 html  
- 焦点与快捷键：文档化「何时键发给 egui / 何时 WebView」  

### 8.4 WebView 宿主两种实现（M0 对比）

| 方案 | 做法 | 优点 | 代价 |
|------|------|------|------|
| **H1 Child 嵌入** | WebView 作为主窗口 child HWND，按阅读区 rect 布局 | z-order 自然、一体 | 与 winit/egui 事件循环抢焦点；DPI/IME/resize 坑多 |
| **H2 跟随顶层窗** | 无边框顶层 WebView 窗，按阅读区屏幕坐标移动缩放 | 消息循环独立，IME/焦点往往更好过 | 需同步位置与 z-order；拖动/resize 可能跟手延迟或闪烁 |

M0 **两种都做最小原型**（或至少 H1 完整测 + H2 半日对比），用 §9.0 打分，选分高者；都挂则切路径 B（Tauri）。

### 8.5 Schema 草案

```sql
CREATE TABLE folders (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  sort_order INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE feeds (
  id INTEGER PRIMARY KEY,
  folder_id INTEGER REFERENCES folders(id) ON DELETE SET NULL,
  url TEXT NOT NULL UNIQUE,
  site_url TEXT,
  title TEXT NOT NULL,
  etag TEXT,
  last_modified TEXT,
  interval_secs INTEGER NOT NULL DEFAULT 3600,
  muted INTEGER NOT NULL DEFAULT 0,
  last_fetched_at INTEGER,
  last_error TEXT,
  sort_order INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE entries (
  id INTEGER PRIMARY KEY,
  feed_id INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
  guid TEXT NOT NULL,
  url TEXT,
  title TEXT,
  author TEXT,
  published_at INTEGER,
  summary TEXT,
  content_cached INTEGER NOT NULL DEFAULT 0,
  is_read INTEGER NOT NULL DEFAULT 0,
  is_starred INTEGER NOT NULL DEFAULT 0,
  fetched_at INTEGER NOT NULL,
  UNIQUE(feed_id, guid)
);

CREATE INDEX idx_entries_feed_pub ON entries(feed_id, published_at DESC);
CREATE INDEX idx_entries_unread ON entries(is_read, published_at DESC);

-- 中文/子串：trigram（需 SQLite FTS5 支持）
CREATE VIRTUAL TABLE entries_fts USING fts5(
  title, author, summary,
  content='entries', content_rowid='id',
  tokenize='trigram'
);
-- 触发器同步 FTS：实施时补全
```

### 8.6 阅读区安全壳

- `resources/reader/shell.html` + 主题 CSS  
- **默认禁用脚本**  
- 内容仅为消毒后 HTML 片段  
- CSP（若有文档模式）：尽量 `default-src 'none'`；图片按策略放行  

---

## 9. 实施计划（里程碑）

### 9.0 M0 — UI 集成 Spike（第一优先级，约 1 周）

**不是：** 四 crate 工程墙、SQLite、CI 全绿、业务订阅。  
**就是：** 证明「壳 + 阅读区」在 Windows 上可长期维护。

#### 9.0.1 Spike 范围

- [ ] 三栏布局（左导航占位 / 中列表占位 / 右阅读区）  
- [ ] WebView **单实例**；连续切换 50 篇 HTML **不**新建实例  
- [ ] H1 child 嵌入与 H2 跟随窗至少对比一套指标  
- [ ] 主题同步（壳背景与 WebView CSS）  
- [ ] 外链 → 系统浏览器  
- [ ] 脚本关闭状态下仍能换文、换主题  

#### 9.0.2 量化验收（失败即计 Fail）

| # | 场景 | 通过标准 | 失败标准 |
|---|------|----------|----------|
| 1 | 拖动分隔条 / 改窗口大小 | 阅读区与占位对齐误差 ≤ 2px；无明显撕裂；持续拖动 CPU 可接受 | 错位、大面积闪白、持续卡顿 |
| 2 | 最大化 / 还原 / 最小化再恢复 | 布局与 WebView 位置正确 | 恢复后空白、错位、残留顶层窗 |
| 3 | 单显示器 100%/125%/150% DPI | 文字与 WebView 不糊、不双缩放 | 明显模糊或坐标翻倍错误 |
| 4 | 跨 DPI 显示器拖移主窗 | 落屏后布局正确（允许短暂自适应） | 永久错位或崩溃 |
| 5 | IME（中文候选）在 **壳** 搜索框 | 候选框位置正常、可上屏 | 候选乱飞/无法输入 |
| 6 | 焦点：列表 ↔ 阅读区 | 点击与快捷键行为符合文档；无「键全吞」 | 焦点死锁、快捷键失效且无恢复手段 |
| 7 | 文内链接点击 | 不导航坏 WebView 会话；外开浏览器 | 卡死、非法协议执行 |
| 8 | 连续 OpenEntry 50 次 | 内存平稳（无每次实例级泄漏） | 私有字节近似线性暴涨 |
| 9 | 跟手性（H2 若采用） | 拖动时阅读区延迟观感 &lt; ~50ms 可接受 | 明显拖影/长时间裸空 |

**门禁：**

- **Pass：** 关键项（1–8）全过；H1/H2 选定一种写入 `docs/spike-ui.md`  
- **Fail：** 任一关键项失败且 1 日内无简单修复 → **停止 Hybrid**，切换 **路径 B（Tauri 2）** 或其它已验证宿主；**不**开始 M1 业务  
- 禁止结论停留在「跑起来了、感觉还行」而无表格打勾  

#### 9.0.3 Spike 交付物

- 可运行实验分支（可后删）  
- `docs/spike-ui.md`：环境、H1/H2 选择、表格结果、录屏或截图、去留决策  

---

### M0b — 工程底板（仅 Spike Pass 后）

- [x] workspace：`glean-core` + `glean-app`  
- [x] `AppCommand` / `AppEvent` 与 `GleanService`  
- [x] SQLite 迁移（folders/feeds/entries + FTS）  
- [x] 基础 CI：fmt + `cargo test -p glean-core`  
- [x] 事件投影到 UI 列表/导航（内存 demo）  

**验证：** 见 `docs/m0b.md`。下一阶段 **M1** 真源抓取。

---

### M1 — 订阅垂直切片

- [ ] 添加 URL、feed-rs 解析、入库  
- [ ] 列表展示；**UnreadChanged / Entry\* 事件驱动**  
- [ ] 单实例 WebView 显示消毒正文  
- [ ] **WebView 关 JS**；**默认 Block 远程图**  
- [ ] 手动刷新 + 源级错误事件  

**验证：** 3 个真实源可读；刷新后未读角标不靠全表轮询；脚本禁用仍可读。

---

### M2 — 阅读与组织

- [ ] 已读/星标/全部已读  
- [ ] 单级文件夹  
- [ ] OPML  
- [ ] FTS5 **trigram** 搜索（中文子串验收）  
- [ ] 磁盘缓存离线读  

**验证：** 断网读缓存；「世界」命中「你好世界」；OPML 往返。

---

### M3 — 打磨

- [ ] 深浅色、布局记忆、快捷键  
- [ ] 并发刷新与错误 UI  
- [ ] 图片策略三档设置  
- [ ] 设置页  

**验证：** §0.4 启动/刷新指标初测。

---

### M4 — 分发

- [ ] 安装包 / 可选便携  
- [ ] 更新检查  
- [ ] 用户手册  
- [ ] 1.0 清单  

---

## 10. 开发规范（摘要）

1. **领域在 core**；UI 只发 Command、归约 Event  
2. **M0 未 Pass 不写业务**  
3. **最小抽象**；插件系统按 §11.5 规划落地（Tier 1 配置优先，Tier 2 Rhai 兜底）  
4. **任何 HTML 入阅读器前消毒**；默认关 JS、默认拦远程图  
5. 密钥与个人 OPML 不进 git  
6. 字符串早键化  
7. 物理 crate 按 §5 折中，不为「架构好看」先拆四个包  

---

## 11. 风险与开放决策

| 风险 | 影响 | 应对 |
|------|------|------|
| egui + WebView 集成 | **项目级** | M0 量化 Spike；Fail → Tauri |
| 无事件主路径 | 状态混乱 | Command/Event 进 M0b/M1 验收 |
| 中文搜索 | 体验差 | 默认 trigram，不靠 unicode61 |
| 远程图追踪 | 隐私 | 默认 Block |
| feed 脚本 | XSS 类 | 关 JS + 消毒 |
| 反爬/Cloudflare | 刷新失败 | 清晰错误；不绕过验证码 |
| SmartScreen | 分发 | 签名 / WinGet |
| 范围膨胀 | 延期 | 里程碑墙；AI/同步进 backlog |

**实施前拍板：**

1. 产品更执着 **尽快可用** 还是 **纯 Rust 壳**？（决定是否跳过 A 直接 Tauri）  
2. 显示名：`Glean` / `拾光` 组合  
3. V1 托盘？便携模式？  
4. 许可证：MIT / Apache-2.0 / 专有  

---

## 11.5 插件系统

### 11.5.1 目标与边界

| 维度 | 决定 |
|------|------|
| **形态** | **配置规则（Tier 1） + Rhai 脚本（Tier 2）**；**不选 Wasm** |
| **形态选择理由** | 站点适配器瓶颈是 I/O（等网络），不是计算，Wasm 的性能优势用不上；Rhai 是纯 Rust 嵌入式脚本，权限模型是白名单式注册宿主函数（`http_get`/`css_select`/`set_field`），拿不到文件系统/任意 socket/进程调用；单人维护场景下 Rhai 的审计边界比 WASI capability 更直观 |
| **首批场景** | ① 网站特殊订阅（Pixiv/Twitter/X/GitHub/Bilibili/YouTube/Fantia/Fanbox）② 内容增强（全文抽取、图片代理、视频嵌入、代码高亮）③ AI 翻译/摘要 ④ 过滤/排序 |
| **何时切换 Wasm** | 当出现「不受信任的第三方作者投稿 + 插件市场」需求时再评估；当前不为想象中的市场提前抽象 |

**能力分层：**

- **Tier 1 — 配置规则（YAML）**：覆盖 80% 场景。URL 匹配 + CSS 选择器 + 字段映射，无脚本。
- **Tier 2 — Rhai 脚本**：覆盖剩余 20% 的逃生舱。分页游标、签名计算、速率限制重试、复杂 HTML 处理。

### 11.5.2 架构与数据流

```
┌──────────────────────────────────────────────────────────┐
│  Glean Host（Rust）                                       │
│                                                          │
│  PluginManager                                           │
│    ├─ load()   ← 扫描 plugins/，解析 manifest.toml       │
│    ├─ match(url) → 命中的插件列表                         │
│    └─ invoke(hook, ctx) → 执行对应 hook                  │
│                                                          │
│  PluginRuntime（Rhai Engine）                            │
│    ├─ 白名单 Host 函数（http_get/css_select/set_field…）  │
│    ├─ 操作数上限 / 超时 / per-plugin 资源配额             │
│    └─ 沙箱：无 FS / 无 socket / 无 process               │
│                                                          │
│  hooks（切入点）                                          │
│    ├─ resolve_feed(url) → FeedConfig                     │
│    ├─ fetch(url, headers) → httpResponse                 │
│    ├─ parse_feed(rawBytes) → FeedItems                   │
│    ├─ enhance_entry(entry) → EnhancedEntry               │
│    ├─ filter_entry(entry) → bool                         │
│    └─ transform_html(html) → html                        │
└──────────────────────────────────────────────────────────┘
```

**数据流（以 Pixiv 为例）：**

```text
用户输入 pixiv.net/user/12345
  → PluginManager.match("pixiv.net") 命中 pixiv.rhai
  → resolve_feed(url) 返回 { feed_url: API 端点, headers: { Cookie: stored } }
  → Host 执行 HTTP fetch（注入 Cookie，走代理）
  → parse_feed(json) 用 Rhai 把 JSON 转 RSS 模型
  → enhance_entry(item) 补全图片 URL、作者信息
  → Host 消毒 + 入库
```

### 11.5.3 插件清单（manifest.toml）

每个插件目录结构：

```
plugins/
  pixiv/
    manifest.toml    # 清单（必需）
    adapter.rhai     # Tier 2 脚本（可选；Tier 1 纯配置时不需要）
    icon.png         # 16×16 图标（可选）
```

`manifest.toml` 示例（Pixiv）：

```toml
[plugin]
id = "pixiv"
name = "Pixiv 订阅适配器"
version = "0.1.0"
author = "Glean"
description = "将 Pixiv 用户作品页转为 RSS"
min_glean_version = "0.4.0"

# 触发规则：URL 匹配此列表时激活
[[match]]
url_pattern = "pixiv.net/users/*"
# 也可按 feed_url 前缀匹配
# url_pattern = "https://app-api.pixiv.net/v1/user/works*"

# 权限声明（安装/更新时向用户展示，能力扩大时需重新确认）
[permissions]
# HTTP 请求范围（Host 强制校验，超出范围直接拒绝）
http_domains = ["app-api.pixiv.net", "www.pixiv.net"]
# 凭证：声明需要的凭证 key，用户在设置里填值
credentials = ["pixiv_cookie"]
# 外部服务调用（AI 翻译等）
external_call = false
# 内容增强：允许 transform_html 改写正文
content_rewrite = true

# Tier 1 配置（简单场景）：如果不需要脚本逻辑，纯配置即可
[config]
# 指定 CSS/JSON 选择器抽取字段
feed_title_selector = "$.user.name"
item_title_selector = "$.illust.title"
item_url_template = "https://www.pixiv.net/artworks/{id}"
```

### 11.5.4 权限模型

**原则：能力原语 + 作用域参数，由 Host 执行并强制校验范围。**

| 能力 | Host 函数 | 作用域参数 | 强制校验 |
|------|-----------|-----------|---------|
| HTTP 请求 | `http_get(url, headers)` / `http_post(url, body, headers)` | `http_domains` 白名单 | 域名不在白名单 → 拒绝；超时/重试按插件单独配额 |
| 凭证访问 | `get_credential(key)` | `credentials` 声明列表 | key 未声明 → 返回空；值由用户在设置填入，脚本只读 |
| 内容改写 | `set_field(name, value)` / `transform_html(html)` | `content_rewrite` 布尔 | 未声明 content_rewrite=true → 改写操作被丢弃 |
| 外部服务 | `external_call(provider, payload)` | `external_call` 布尔 + provider 白名单 | 未声明 → 拒绝；provider 未注册 → 拒绝 |
| CSS/JSON 提取 | `css_select(html, selector)` / `json_path(json, path)` | 无限制（纯计算） | 操作数计入上限 |

**安全红线：**

1. **安装/更新时权限确认**：manifest 声明的能力在安装时给用户展示摘要（"这个插件将：访问 pixiv.net / 使用你保存的 pixiv 登录凭据"），用户确认才生效。**插件更新时如果声明的能力集合变大（如新版多要了 external_call），必须重新弹出确认**——防止"先申请无害权限过审，后续偷偷加大"的供应链攻击。
2. **执行上限**：每个 Rhai 脚本有操作数上限（`engine.set_max_operations()`）和超时上限，防止死循环或恶意卡死刷新流程；`http_request` 的超时和重试退避按插件单独算，一个插件卡住不拖慢全局刷新。
3. **UI 扩展限制**：**不开放"注入任意 JS/CSS 到阅读区"这条路**——它会废掉已定的"WebView 禁用 JS"安全决策。UI 扩展只能用声明式 `action`/`embed` 白名单（如"在条目底部插入翻译按钮"由 Host 渲染，不是插件自由注入脚本）。

### 11.5.5 Rhai Host 函数清单（首批 API）

```rust
// === HTTP（作用域：http_domains） ===
/// GET 请求，返回 { status, headers, body }
fn http_get(url, headers_map) -> Map;
/// POST 请求
fn http_post(url, body_string, headers_map) -> Map;
/// URL 编码
fn url_encode(string) -> String;

// === 解析（纯计算，无限制） ===
/// CSS 选择器提取，返回匹配元素的文本/HTML
fn css_select(html_string, selector) -> Array;
/// JSON Path 提取（$.store.book[0].title）
fn json_path(json_string, path) -> Dynamic;
/// 正则提取
fn regex_extract(string, pattern) -> Array;

// === 凭证（作用域：credentials 声明） ===
/// 获取用户填写的凭证值（未声明 key 返回空字符串）
fn get_credential(key) -> String;

// === 条目操作（作用域：content_rewrite / 无限制读取） ===
/// 设置条目字段（title/url/author/content_html/summary/published_at）
fn set_field(name, value);
/// 追加内容到正文
fn append_html(html_fragment);

// === 外部服务（作用域：external_call + provider 白名单） ===
/// 调用注册的外部服务（如 translate/summarize）
fn external_call(provider, payload_map) -> Map;

// === 工具 ===
fn log(level, message);  // 写入插件日志
fn now() -> i64;         // Unix 时间戳
fn hash(string) -> String; // SHA-256
```

### 11.5.6 Hook 切入点

| Hook | 触发时机 | 签名 | 用途 |
|------|---------|------|------|
| `resolve_feed` | 添加订阅时，URL 匹配后 | `fn resolve_feed(url) -> FeedConfig` | 返回真实 feed 端点、所需 headers |
| `fetch`（Host 执行） | 刷新时 | — | Host 按 `resolve_feed` 返回值执行 HTTP |
| `parse_feed` | HTTP 响应到达后 | `fn parse_feed(raw_bytes) -> Array<Entry>` | 把非标准格式（JSON API）转成条目列表 |
| `enhance_entry` | 入库前 | `fn enhance_entry(entry) -> Entry` | 补全图片、作者、标签等 |
| `transform_html` | 阅读器渲染前 | `fn transform_html(html) -> String` | 代码高亮、图片代理、视频嵌入 |
| `filter_entry` | 列表渲染前 | `fn filter_entry(entry) -> bool` | 过滤不感兴趣的条目 |
| `ai_enhance` | 手动/自动触发 | `fn ai_enhance(entry, provider) -> Map` | 翻译/摘要，结果写回 content_extracted |

### 11.5.7 插件示例

#### Pixiv 适配器（Tier 2 Rhai 脚本）

```rhai
// adapter.rhai

fn resolve_feed(url) {
    // 从 pixiv.net/users/12345 提取 user_id
    let m = regex_extract(url, "users/(\\d+)");
    if m.len() == 0 {
        log("error", "无法解析 Pixiv 用户 ID");
        return #{};  // 空表示放弃
    }
    let user_id = m[0];
    let cookie = get_credential("pixiv_cookie");
    #{
        feed_url: "https://app-api.pixiv.net/v1/user/works?user_id=" + user_id + "&type=illust",
        headers: #{
            "Authorization": "Bearer MHZk...（公开客户端 token）",
            "Cookie": cookie,
            "User-Agent": "PixivIOSApp/7.13",
        },
    }
}

fn parse_feed(raw_bytes) {
    let json = parse_json(raw_bytes.to_string());
    let items = [];
    for illust in json.illusts {
        items.push(#{
            title: illust.title,
            url: "https://www.pixiv.net/artworks/" + illust.id,
            author: illust.user.name,
            content_html: build_image_html(illust),
            published_at: parse_date(illust.create_date),
        });
    }
    items
}

fn build_image_html(illust) {
    let html = "<div>";
    for page in illust.meta_pages {
        let img_url = page.image_urls.original;
        html += `<img src="${img_url}" />`;
    }
    html += "</div>"
}
```

#### AI 翻译插件（manifest + 极简脚本）

```toml
# manifest.toml
[plugin]
id = "ai-translate-deepl"
name = "DeepL 翻译"
version = "0.1.0"

[[match]]
url_pattern = "*"  # 所有条目

[permissions]
external_call = true
content_rewrite = true
credentials = ["deepl_api_key"]

[external_providers]
deepl = "https://api-free.deepl.com/v2/translate"
```

```rhai
// adapter.rhai
fn ai_enhance(entry, provider) {
    if provider != "deepl" {
        return #{};
    }
    let key = get_credential("deepl_api_key");
    let result = external_call("deepl", #{
        text: entry.title,
        target_lang: "ZH",
        api_key: key,
    });
    #{
        title_translated: result.translated_text,
    }
}
```

### 11.5.8 存储与分发

```
%APPDATA%\Glean\
  plugins\                    # 用户安装的插件
    pixiv\
      manifest.toml
      adapter.rhai
    ai-translate-deepl\
      manifest.toml
      adapter.rhai
  plugins_config.json         # 各插件的开关状态、凭证值（加密存储）
```

**分发方式（V1）：**

- 手动放置目录安装；设置面板里启用/禁用、填凭证
- 不做插件市场（当前需求不需要；未来若做，需重新评估 Wasm 切换）

**凭证存储：**

- 凭证值（如 Cookie、API Key）由用户在设置面板填入，存入 `plugins_config.json`
- V1 明文存储（与 config.toml 一致，本地优先软件的取舍）；V2 考虑 DPAPI 加密

### 11.5.9 实施计划

| 阶段 | 内容 | 优先级 |
|------|------|--------|
| **P0** | PluginManager 框架：manifest 解析、match、load/unload；Rhai runtime + 白名单 Host 函数；设置面板 UI（列表/启用禁用/凭证填写） | M2 |
| **P0** | `resolve_feed` + `parse_feed` + `enhance_entry` 三个核心 hook；写一个真实插件验证（建议先做 GitHub Releases，API 标准无需凭证） | M2 |
| **P1** | Bilibili / YouTube 适配器（需处理 API key 或 RSSHub 中转） | M2 |
| **P1** | `transform_html` hook（视频嵌入、代码高亮） | M2 |
| **P2** | Pixiv / Twitter / Fantia / Fanbox 适配器（需凭证管理、反爬处理） | M3 |
| **P2** | AI 翻译/摘要：`ai_enhance` hook + external_call 机制 + provider 注册 | M3 |
| **P2** | `filter_entry` hook（过滤/排序） | M3 |

### 11.5.10 开发规范

1. **Tier 1 优先**：能用配置规则解决的场景不写 Rhai 脚本（减少攻击面）
2. **最小权限**：manifest 只声明必需的能力；`http_domains` 尽可能窄
3. **幂等性**：所有 hook 必须幂等（同一输入多次调用结果一致），因为刷新可能重试
4. **不阻塞主循环**：所有 hook 在 worker 线程执行；超时由 Host 强制
5. **日志隔离**：每个插件有独立日志前缀 `[plugin:id]`，方便排查
6. **沙箱审计清单**：新增 Host 函数时必须回答三个问题——它能访问什么资源？作用域参数是什么？超出作用域时 Host 如何拒绝？

---

## 12. 附录

### A. 依赖（示意）

```toml
# Hybrid 路径；Tauri 路径则 UI 依赖替换为 tauri
eframe = "…"
egui = "…"
wry = "…"   # 或 webview2 / tauri
tokio = { version = "…", features = ["rt-multi-thread", "macros", "time"] }
reqwest = { version = "…", default-features = false, features = ["rustls-tls", "gzip", "brotli", "http2"] }
feed-rs = "…"
rusqlite = { version = "…", features = ["bundled"] }
serde = { version = "…", features = ["derive"] }
tracing = "…"
ammonia = "…"
```

### B. 参考

- [Folo](https://github.com/RSSNext/Folo)  
- RSS / Atom / JSON Feed / OPML  
- WebView2：`IsScriptEnabled`、导航拦截  
- SQLite FTS5 trigram tokenizer  

### C. 若只能先做三件事

1. **M0 WebView 集成 Spike** + §9.0.2 量化门禁与 H1/H2 对比  
2. **中文搜索默认 trigram**（跳过 jieba）  
3. **关 JS + 默认拦截远程图**（M1 写进代码，不做「后期安全专项」）  

### D. 文档维护

- Spike 结论写入 `docs/spike-ui.md` 并回写本节路径 A/B 选择  
- 核心/feed/store **在切换 UI 时保持可复用**  
- 版本史：0.1.0 初稿 → 0.2.0 评审修订 → **0.2.1 锁定路径 A**  

---

**进度：** M0 有条件通过 · M0b 完成 · **M1 完成**（`docs/m1.md`）。  
**下一步：** 持久化目录、异步刷新、OPML、搜索接线 UI。
