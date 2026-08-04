# Glean / 拾光 — 完整开发方案

> 纯 Windows 本地优先的现代化 RSS / 信息流聚合阅读器  
> 参考产品：[RSSNext/Folo](https://github.com/RSSNext/Folo)  
> 文档版本：0.5.2 · 日期：2026-08-04  
> 修订说明：**M0–M5 主线全部落地**（订阅/阅读/组织/搜索/离线/打磨/插件框架，137 测试绿）；**M6 大部分完成**（Pixiv 适配器、AI 增强可读展示、DPAPI/keyring 凭证加密）；开发过程中追加的临时需求（UI/数据可靠性/刷新限流/停止刷新/AI 展示修复等）已整理进 **§13**。  
> 剩余未完成：M4 正式安装包与用户手册、§0.4 性能指标实测、M6 安装时权限确认 UI、M7 的 Twitter/X、Fantia、Fanbox 适配器。

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

- [x] 三栏布局（左导航占位 / 中列表占位 / 右阅读区）  
- [x] WebView **单实例**；连续切换 50 篇 HTML **不**新建实例  
- [x] H1 child 嵌入与 H2 跟随窗至少对比一套指标（债：H2 未真正分叉，见 spike-ui.md）  
- [x] 主题同步（壳背景与 WebView CSS）  
- [x] 外链 → 系统浏览器  
- [x] 脚本关闭状态下仍能换文、换主题  

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

- [x] 可运行实验分支（已并入主分支）  
- [x] `docs/spike-ui.md`：环境、H1/H2 选择、表格结果、去留决策  

---

### M0b — 工程底板（仅 Spike Pass 后）

- [x] workspace：`glean-core` + `glean-app`  
- [x] `AppCommand` / `AppEvent` 与 `GleanService`  
- [x] SQLite 迁移（folders/feeds/entries + FTS）  
- [x] 基础 CI：fmt + `cargo test -p glean-core`  
- [x] 事件投影到 UI 列表/导航（内存 demo）  

**验证：** 见 `docs/m0b.md`。下一阶段 **M1** 真源抓取。

---

### M1 — 订阅垂直切片（已落地）

- [x] 添加 URL、feed-rs 解析、入库  
- [x] 列表展示；**UnreadChanged / Entry\* 事件驱动**  
- [x] 单实例 WebView 显示消毒正文  
- [x] **WebView 关 JS**；**默认 Block 远程图**  
- [x] 手动刷新 + 源级错误事件

**验证：** 3 个真实源可读；刷新后未读角标不靠全表轮询；脚本禁用仍可读。

---

### M2 — 阅读与组织（已落地）

- [x] 已读/星标/全部已读  
- [x] 单级文件夹  
- [x] OPML  
- [x] FTS5 **trigram** 搜索（中文子串验收）  
- [x] 磁盘缓存离线读

**验证：** 断网读缓存；「世界」命中「你好世界」；OPML 往返。

---

### M3 — 打磨（已落地，指标待实测）

- [x] 深浅色、快捷键  
- [x] 布局记忆（窗口位置/大小/最大化持久化到 config.json，启动时恢复）  
- [x] 并发刷新与错误 UI  
- [x] 图片策略三档设置（Block / LoadOnDemand / Allow）  
- [x] 设置页（含 AI 增强、缓存目录、插件管理入口；关闭按钮在标题栏右上角）  
- [x] 主题色色板 + 弹窗几何（设置/插件/错误日志/OPML）持久化  
- [x] 托盘（Windows，最小化到托盘 + 直接 Win32 恢复窗口）、单实例（CreateMutexW）  
- [ ] §0.4 性能指标实测（冷启动/内存/100 源刷新/滚动 fps）

**验证：** §0.4 启动/刷新指标初测。（尚未实测）

---

### M4 — 分发（部分完成）

- [ ] 安装包 / 可选便携（脚本就绪：package-inno.iss / portable-mode.txt；CI 已自动产出 Windows Release EXE + Inno Setup 安装器，尚未发布正式 Release）  
- [x] 更新检查（appcast.json，prompt-only）  
- [x] GitHub Actions Windows 构建流水线（push main 自动构建）  
- [ ] 用户手册  
- [ ] 1.0 清单  

### M5 — 插件系统框架（已落地）

- [x] Tier 0 内置：GitHub `releases.atom`、YouTube channel XML 的 URL 规范化（`feed/tier0.rs`，13 测试）  
- [x] `PluginManager` 框架：扫描 `<data_dir>/plugins/`、解析 `manifest.toml`、URL glob 路由（`plugin/manager.rs`）  
- [x] `manifest.toml` serde 结构：`[plugin]`/`[[match]]`/`[capabilities]`/`[compliance]`/`[tier1]`（`plugin/manifest.rs`）  
- [x] Rhai runtime：按插件 manifest 动态构建 `Engine`，操作数上限 + 调用栈上限 + `disable_symbol(eval/import/export/Fn)`；按能力原语注册 `now`/`log`/`parse_json`/`json_path`/`http_get`/`http_post`/`set_field`/`set_embed`（`plugin/runtime.rs`）  
- [x] Tier 1 配置引擎：URL 模板替换 + 域名白名单 + JSON 路径字段映射（`plugin/tier1.rs`）  
- [x] 凭证存储抽象：`CredentialStore` + `EncryptedBlob`（`scheme = "plaintext-stub"`，DPAPI/keyring 推到 M6）  
- [x] 凭证零接触：`http_get` 内部 Host 注入 Header，Rhai 脚本永远拿不到明文  
- [x] 能力原语强制：未声明 `feed_fetch` 的插件，脚本里 `http_get` 直接报"函数不存在"  
- [x] 集成到 `GleanService`：`open_path_with_proxy` 加载插件目录 + 凭证存储；`plugins()`/`credentials()`/`credentials_mut()` 访问器  
- [x] 测试：40 旧测 + 40 新测全过；`cargo check` (core + app) + `cargo clippy` 新代码零警告 + `cargo fmt --check` 干净  

**M5 范围之外（已随 M6 落地）**：Bilibili / Pixiv 端到端验证；Tier 2 `EntryCollector` 接入；Enhancer 接口；DPAPI/keyring 实际加密。**尚未做**：manifest 安装时能力确认 UI（见 M6）。

---

### M6 — 站点适配器与增强（大部分完成）

- [x] Tier 2 `EntryCollector` 接入（`set_field`/`add_entry`/`set_feed_title`/`set_embed`，自动 commit）  
- [x] **Pixiv 适配器**（Tier 2 Rhai，`plugins/pixiv/`）：OAuth refresh_token 换 access_token（带 3 次重试）、`include_policy=true`、按 `next_url` 分页拉满 20 页、app-api JSON 字段映射、i.pximg.net 图片带 Referer  
- [x] **Bilibili 适配器**（Tier 1 + Rhai）：manifest + `wbi_sign` 签名、投稿列表解析（`plugins/bilibili/`）  
- [x] URL 规范化扩展（`feed/tier0.rs`）：新增 Pixiv 单数 `user/{id}` → 复数 `users/{id}`，并在单源刷新与批量刷新 worker 双路径 fallback  
- [x] **HTTP 429 限流重试**：指数退避（2/4/8/16s，最多 4 次重试）；`REFRESH_WORKERS` 降为 1（串行刷新）  
- [x] **Enhancer 接口 + AI 增强**：摘要 / 翻译（OpenAI 兼容 API，Key 由设置持有、加密存储，插件不可读）；`entry_enhancements` 表（schema v9）；阅读区 `.ai-enhancement` 展示；`ai_translate_lang` 可配置；消毒保留 `class`  
- [x] **凭证加密存储**：Windows DPAPI（`CryptProtectData`）+ Linux keyring（secret-service + AES-256-GCM）；`EncryptedBlob.scheme` 驱动解密分发  
- [x] 凭证注入：Rhai `{{slot}}` body 占位符替换 + 声明式 Header 注入（`credential_use` 白名单强制）  
- [x] 插件管理 UI：安装/卸载/启用停用、凭证槽编辑（含 Header name / Credential value）、代理设置持久化（`AppConfig.plugin_proxy`）；窗口最大高度 720px 内部滚动  
- [ ] **manifest 安装时能力确认 UI**（§11.5.4：安装/更新展示能力摘要、能力扩大需重新确认）

**未完成（M7+）：** Twitter/X、Fantia、Fanbox 适配器。

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

> **排期声明：本节为路线图设计，排到 M0–M4（能读到第一条真实 feed）之后的 M5/M6 阶段。**
> 不让它拖慢核心垂直切片的验证。先有能用的阅读器，再谈插件。

### 11.5.1 这不是一个插件系统，是两个

用户用例其实分两类，风险和接口完全不同，**不应共用同一套形态**：

| 类别 | 职责 | 接口形态 | 信任级别 |
|------|------|---------|---------|
| **A. 站点适配器** | 把非标准网站（Pixiv/Twitter/Bilibili 等）变成一批 Entry；插在 `glean-feed` 前面 | Tier 0/1/2 分层（见下） | 需要网络 + 可能需凭证，高信任 |
| **B. 功能增强器（Enhancer）** | Entry 已入库后对字段做修改/追加（翻译、摘要） | 独立小接口，不接触原始网络请求 | 低信任，能力面小 |

### 11.5.2 站点适配器：分三层，不是一种形态

7 个目标网站情况差异极大，分三层处理：

| 层 | 覆盖对象 | 形态 | 理由 |
|----|---------|------|------|
| **Tier 0（内置，不算插件）** | GitHub（`releases.atom`）、YouTube（channel XML） | `glean-feed` 里的 URL 规范化/重写，ship 在核心代码里 | 本来就有官方 RSS，几行代码，不值得设计插件契约 |
| **Tier 1（纯配置）** | Bilibili 等结构稳定的半公开 JSON API | TOML：URL 匹配 + 请求模板 + JSON/CSS 字段映射 | 结构稳定，配置即可覆盖 |
| **Tier 2（Rhai 脚本）** | Twitter/X、Pixiv、Fantia、Fanbox | 沙箱脚本，见 §11.5.4 | 需要分页游标、签名计算、Cookie 注入、速率限制重试 |

> **合规声明**：Fantia/Fanbox 是付费创作者平台。适配器只使用**用户自己的登录态**访问**用户自己有权限看到的内容**，不绕过付费墙、不做验证码绕过、不做批量抓取放大攻击风险。这与文档已有的「不绕过验证码」原则一致。

### 11.5.3 形态选择：Rhai，不选 Wasm

- 站点适配器瓶颈是 **I/O**（等网络返回），不是计算，Wasm 的性能优势用不上。
- **Rhai 是纯 Rust 嵌入式脚本引擎，权限模型是白名单式注册宿主函数**——Host 精确决定脚本能调用哪些 Rust 函数，脚本拿不到文件系统、任意 socket、进程调用。对单人维护者来说，审计边界比 WASI capability 更直观。
- 当出现「不受信任的第三方作者投稿 + 插件市场」需求时再评估 Wasm；当前不为想象中的市场提前抽象。

### 11.5.4 权限模型：能力原语 + 作用域参数

**核心原则：插件声明意图，Host 执行并强制校验范围。**

> **措辞纠正**：不用「HTTP 拦截/重写」这个词——它暗示「全局中间件能看到所有请求」。
> 正确模型：**每个 Feed 绑定一个 `adapter_id`，刷新该 Feed 时由该适配器全权代劳，但适配器永远看不到、碰不到其他 Feed 的请求。**

Rhai 的 `Engine` 实例**按插件动态构建**，只注册该插件声明过的 Host 函数。没声明 `credential.use:pixiv_session` 的插件，脚本里写 `get_credential("pixiv_session")` 会直接报「函数不存在」，而不是「权限不足」——能力边界是代码层面强制的。

| 能力原语 | 作用域参数 | 强制校验 |
|---------|-----------|---------|
| `feed.fetch` | 域名白名单（如 `app-api.pixiv.net`, `i.pximg.net`） | 域名不在白名单 → Host 拒绝；超时/重试 per-plugin 配额 |
| `credential.use:<slot>` | 具名凭证槽（如 `pixiv_session`） | **Host 在请求发出前注入 Header，Rhai 脚本永远拿不到明文值** |
| `content.transform` | 只能写入白名单字段（见 §11.5.6） | 输出**必须回流经过 ammonia 消毒**，不能绕过 |
| `external.call:<domain>` | 具体服务域名（如 `api.deepl.com`） | **Key 由 Glean 设置持有，Host 调用时注入，插件脚本拿不到 Key** |
| `css_select` / `json_path` / `regex` | 纯计算，无限制 | 操作数计入上限 |

#### 凭证永远不进插件手里（关键安全决策）

这是对「AI 翻译」「站点适配器」共同适用的红线：

1. 用户在 Glean 设置里粘贴一次 Cookie/API Key
2. Glean 用 **DPAPI**（`windows` crate 的 `CryptProtectData`，或 `keyring` crate）加密存储到本地
3. 插件 manifest 声明 `credential.use:pixiv_session`
4. **Host 在真正发起 `http_request` 之前把 Header 塞进去——Rhai 脚本只写了「请给这个请求附上 pixiv_session 凭证」，从没摸到过明文值**

这样即使适配器脚本被换成恶意版本，它能干的坏事最多是「拿这个凭证发一次它声明过域名范围内的请求」，不可能把 Cookie 整体带出去发给别的服务器。

#### 安装/更新时权限确认 + 执行上限

1. manifest 声明的能力在安装时给用户展示摘要，用户确认才生效。**插件更新时如果能力集合变大（如新版多要了 `external.call`），必须重新弹出确认**——防供应链攻击。
2. 每个 Rhai 脚本有操作数上限（`engine.set_max_operations()`）和超时上限，防止死循环或卡死刷新流程。

### 11.5.5 内容增强：结构化引用，不直接写 HTML

**B 站嵌入播放器这类需求，插件不能直接往正文塞 `<iframe>`**——那等于绕过 ammonia 消毒关卡。

正确做法：插件产出**结构化引用**，Host 用固定模板渲染：

```
embed_ref: { provider: "bilibili", bvid: "BV1xx411c7mD" }
```

Host 只认几个**预先审核过的 provider**（B 站、YouTube），用固定模板 + 严格 `sandbox` 属性生成 `<iframe>`。插件永远不能自己决定 iframe 的 src 或往里面塞脚本。这与主流阅读器处理 oEmbed 的思路一致。

翻译/全文抽取同理：插件产出的文本写回 Entry 前**一样过一遍消毒管线**，不能因为「这是认证过的插件」就跳过。

### 11.5.6 Rhai Host 函数清单（Tier 2 站点适配器）

```rust
// === HTTP（作用域：feed.fetch + 域名白名单）===
/// 发起请求，Host 注入声明的凭证到 Header（脚本拿不到明文）
/// credential_slot: 声明过的具名槽，如 "pixiv_session"
fn http_get(url, headers_map, credential_slot) -> Map;  // {status, headers, body}
fn http_post(url, body, headers_map, credential_slot) -> Map;

// === 解析（纯计算）===
fn css_select(html, selector) -> Array;
fn json_path(json, path) -> Dynamic;
fn regex_extract(string, pattern) -> Array;
fn parse_json(string) -> Dynamic;

// === 条目构建（输出必须回流过 ammonia）===
/// 设置字段（白名单：title/url/author/summary/published_at/embed_ref）
fn set_field(name, value);
/// 声明 embed 引用（Host 用固定模板渲染，见 §11.5.5）
fn set_embed(provider, id);

// === 工具 ===
fn log(level, message);
fn now() -> i64;
```

**注意：没有 `get_credential()`。** 凭证由 Host 在 `http_get` 内部注入，脚本无法读取明文。

### 11.5.7 功能增强器（Enhancer）——独立小接口

AI 翻译/摘要这类**不需要 Tier 0-2 那套抓取权限**，用更小的独立契约：

```rust
/// 增强器 trait（Rust 原生或 Rhai 实现）
trait Enhancer {
    fn id(&self) -> &str;
    fn applies_to(&self, entry: &Entry) -> bool;
    /// 通过 host_api 按需调用外部服务（如翻译），API Key 由 Host 持有
    fn enhance(&self, entry: &Entry, host: &HostApi) -> Result<EntryPatch>;
}

/// Host 提供给增强器的受限 API
trait HostApi {
    /// 调用翻译服务（Key 由 Glean 设置持有，增强器拿不到）
    fn call_translation(&self, text: &str, target_lang: &str) -> Result<String>;
    /// 调用摘要服务
    fn call_summarize(&self, text: &str) -> Result<String>;
}
```

**API Key 不交给插件本身持有**——用户在 Glean 设置里配置好 Key，插件运行时通过 `HostApi` 按需调用。即使第三方插件脚本被换成恶意版本，能造成的伤害也只是「滥用这一次调用」，而不是「把用户的 API Key 整个偷走」。

### 11.5.8 插件清单（manifest.toml）

```
plugins/
  pixiv/
    manifest.toml    # 清单（必需）
    adapter.rhai     # Tier 2 脚本（Tier 1 纯配置时不需要）
    icon.png         # 16×16（可选）
```

```toml
[plugin]
id = "pixiv"
name = "Pixiv 订阅适配器"
version = "0.1.0"
author = "Glean"
min_glean_version = "0.5.0"
tier = 2  # 0=内置 / 1=配置 / 2=脚本 / enhancer=功能增强

[[match]]
url_pattern = "pixiv.net/users/*"

[capabilities]
# 能力原语 + 作用域（Host 强制校验）
feed_fetch = ["app-api.pixiv.net", "i.pximg.net"]
credential_use = ["pixiv_session"]   # Host 注入，脚本不可读
content_transform = ["embed_ref"]    # 只能写白名单字段

[compliance]
# 合规声明：只访问用户自己有权限的内容
uses_user_session = true
```

### 11.5.9 凭证存储：DPAPI 加密

凭证（Cookie、API Key）是高度敏感数据，**不能明文扔进 SQLite 或配置文件**。

- Windows 上用 **DPAPI**（`CryptProtectData`）做机器级加密存储
- `windows` crate 已有依赖，或用 `keyring` crate
- **任何插件都不能自己读写另一个插件的凭证，凭证由宿主统一加密存储和按需注入**

### 11.5.10 UI 扩展：V1 不做，或仅声明式

**V1 不开放任意 JS/CSS 注入到阅读区**——它会废掉已定的「WebView 禁用 JS」安全决策。插件 JS 一旦能在 WebView 跑，就有该 WebView 实例能做的一切事。

如果确实需要「侧栏菜单」「翻译按钮」这类扩展点，改成**声明式**而非代码式：

```toml
[[ui.actions]]
label = "翻译为中文"
on_click = { action = "enhance", enhancer = "deepl-translate" }
```

插件只能从 Host 定义好的 action 词汇表里选，真正的 UI 元素由 Glean 渲染。插件描述「要什么」，不执行「怎么做」。

### 11.5.11 实施路线图（M5/M6）

| 阶段 | 内容 |
|------|------|
| **M5** | Tier 0 内置：GitHub `releases.atom`、YouTube channel XML 的 URL 规范化 |
| **M5** | PluginManager 框架 + Rhai runtime + DPAPI 凭证存储 |
| **M5** | Tier 1 配置 + Tier 2 脚本核心 hook（resolve/parse/enhance） |
| **M6** | Bilibili 适配器（Tier 1）；验证端到端 |
| **M6** | Enhancer 接口 + AI 翻译（DeepL/OpenAI）|
| **M7+** | Twitter/X、Pixiv、Fantia、Fanbox 适配器（需凭证、反爬处理） |

### 11.5.12 开发规范

1. **Tier 0 优先**：有官方 RSS 的网站（GitHub/YouTube）写进核心，不做成插件
2. **Tier 1 优先于 Tier 2**：能用配置解决的场景不写脚本，减少攻击面
3. **最小权限**：manifest 只声明必需能力；`feed_fetch` 域名白名单尽可能窄
4. **凭证零接触**：插件永远拿不到明文凭证，由 Host 注入
5. **输出回消毒管线**：插件产出的所有 HTML/文本写回 Entry 前必过 ammonia
6. **幂等性**：所有 hook 幂等（刷新可能重试）
7. **不阻塞主循环**：所有 hook 在 worker 线程，超时由 Host 强制
8. **适配器失效 UI**：目标网站改版导致脚本失效时，明确提示用户而非静默失败

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
- 版本史：0.1.0 初稿 → 0.2.0 评审修订 → 0.2.1 锁定路径 A → 0.5.0 整理迭代追加需求（§13）→ 0.5.1 AI 展示修复（消毒保留 class / 翻译语言 / 忙时提示）

---

## 13. 迭代追加需求（2026-07/08 开发过程中新增）

> 以下需求来自开发迭代中的用户反馈，多数不在原始里程碑内，落地后登记于此，保证计划书与实际实现一致。

### 13.1 UI / 交互

| # | 需求 | 落地说明 | 状态 |
|---|------|---------|------|
| 1 | 添加订阅处分类下拉菜单不出现滚动条 | 加宽 ComboBox 至 150 + popup min-width | ✅ 已实现 |
| 2 | 导航区多选/非多选行高统一 | 两种模式统一用 `SelectableLabel` 同高渲染 | ✅ 已实现 |
| 3 | 拖拽订阅时收起未选订阅、只显示文件夹 | 拖拽前保存 `expanded_folders`，释放后恢复 | ✅ 已实现 |
| 4 | 滚动条与分隔条贴合（消除间隙） | `bar_outer_margin=0` + `auto_shrink` | ✅ 已实现 |
| 5 | 悬停默认箭头、仅拖拽时 grab 光标 | `on_hover_cursor(Default)` 覆盖 dnd 的 Grab | ✅ 已实现 |
| 6 | 非多选时零宽勾选框占位保持行高 | `add_sized((0, row_height), SelectableLabel)` | ✅ 已实现 |
| 7 | 多选工具栏：全选 / 反选 / 取消选择 | 多选模式下工具栏按钮 | ✅ 已实现 |
| 8 | 列表区行点击修复（可点、无文本选择态） | 行级 `Frame` + `interact(Sense::click())` | ✅ 已实现 |
| 9 | 设置页关闭按钮移到标题栏右上角 | 启用标题栏 x，移除底部关闭按钮 | ✅ 已实现 |
| 10 | 导航区订阅拖拽排序 | 「整理」模式（与拖入文件夹互斥），组内行间插入 + 指示线；`feeds.sort_order`（schema v13）持久化 | ✅ 已实现 |
| 11 | 主题色 / 窗口几何 / 弹窗几何跨会话记忆 | `AppConfig`：accent、window pos/size/maximized、settings/plugins/errors/opml 弹窗几何 | ✅ 已实现 |
| 12 | 列表虚拟滚动空白区 / 滚动条错位修复 | `ScrollArea::show_rows` 只渲染可见行 | ✅ 已实现 |
| 13 | 插件管理窗口最大高度 + 内部滚动 | 720px 上限，防无限向下扩展 | ✅ 已实现 |
| 14 | 批量代理开关（选中订阅统一开启/关闭） | 多选工具栏「开启代理 / 关闭代理」 | ✅ 已实现 |
| 32 | 缩略图大小上限提高到 120px | `THUMBNAIL_SIZE_MAX` 60→120；滑块/输入同步放宽 | ✅ 已实现 |

### 13.2 数据可靠性

| # | 需求 | 落地说明 | 状态 |
|---|------|---------|------|
| 15 | 应用内数据库修复 + 强制重建 | 设置页「修复数据库」：WAL checkpoint + integrity_check + FTS5 rebuild + 全表扫描；「强制重建」从 `.db.bak` 恢复 | ✅ 已实现 |
| 16 | 启动时数据库损坏自动检测与恢复 | `integrity_check` 失败时从 `.db.bak` 恢复 | ✅ 已实现 |
| 17 | 删除订阅保留星标条目 | `entries.feed_id` 改 `ON DELETE SET NULL`（schema v8） | ✅ 已实现 |
| 18 | 新订阅追加到同组排序末尾 | `add_feed` 计算 `MAX(sort_order)+1` | ✅ 已实现 |

### 13.3 刷新 / 网络 / 插件

| # | 需求 | 落地说明 | 状态 |
|---|------|---------|------|
| 19 | 导入的 Pixiv 订阅 404（单数 `user/` URL） | tier0 规范化 `user/{id}`→`users/{id}`；单源与批量 worker 双路径 fallback；manifest 补单数匹配规则 | ✅ 已实现 |
| 20 | Pixiv 刷新 429 限流 | 插件 HTTP 层指数退避重试（2/4/8/16s×4）；`REFRESH_WORKERS` 6→1 | ✅ 已实现 |
| 21 | 每订阅代理开关 | `feeds.use_proxy`（schema v11）+ 设置页全局代理 | ✅ 已实现 |
| 22 | 插件级代理开关 + 代理 URL 自动规范化校验 | `AppConfig.plugin_proxy`；保存时补 `http://` 前缀 + 校验反馈 | ✅ 已实现 |
| 23 | Pixiv OAuth 抖动重试 | 3 次尝试、1s 间隔 | ✅ 已实现 |
| 24 | i.pximg.net 图片 403 | 图片缓存请求带 Referer 头 | ✅ 已实现 |
| 25 | 插件凭证注入（body 占位符 + Header） | `{{slot}}` 替换 + `credential_use` 声明式 Header 注入 | ✅ 已实现 |
| 33 | 刷新后台化 + 停止刷新按钮 | 刷新本就跑在 worker 线程（DB 写回 UI 线程）；「刷新全部」旁新增「停止刷新」：`Arc<AtomicBool>` 取消标志，worker 在下一订阅前退出 | ✅ 已实现 |

### 13.4 构建 / 分发 / 调试

| # | 需求 | 落地说明 | 状态 |
|---|------|---------|------|
| 26 | GitHub Actions Windows 构建 + 安装器 | push main 自动 Release EXE + Inno Setup 安装包 | ✅ 已实现 |
| 27 | `cargo fmt` CI 修复 | rustfmt 格式化对齐 | ✅ 已实现 |
| 28 | 本地调试日志 | `glean-debug.log`（可执行目录），记录配置加载/刷新分页等 | ✅ 已实现 |

### 13.5 AI / 阅读区

| # | 需求 | 落地说明 | 状态 |
|---|------|---------|------|
| 29 | AI 摘要/翻译点击后内容区无变化 | 根因：ammonia 默认剥 `class`，`.ai-enhancement` 样式失效；`generic_attributes` 保留 `class` + 回归测试 | ✅ 已实现 |
| 30 | 翻译目标语言可配置 | `AppConfig.ai_translate_lang`（默认「中文」）；设置页输入；翻译按钮读配置 | ✅ 已实现 |
| 31 | AI 任务进行中静默无反馈 | 已有增强任务时状态栏提示「AI 任务进行中，请稍候…」 | ✅ 已实现 |

---

**进度：** M0 有条件通过 · M0b 完成 · M1/M2 完成 · **M3 已落地（指标待实测）** · M4 部分完成（安装包/手册待产） · **M5 完成** · **M6 大部分完成** · M7+ 待做（Twitter/X、Fantia、Fanbox）。  
**下一步（按优先级）：** ① §0.4 性能指标实测；② M4 正式安装包发布与用户手册；③ M6 安装时权限确认 UI；④ M7 适配器评估（Twitter/X、Fantia、Fanbox）。
