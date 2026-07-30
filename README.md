# Glean / 拾光

Windows 本地优先的现代化 RSS / 信息流聚合阅读器。

> **当前阶段：M1 订阅垂直切片**
> HTTP + feed-rs 入库；阅读区 WebView2；路径 A（egui Hybrid）。

- 开发方案：[`docs/Glean-开发方案.md`](docs/Glean-开发方案.md)
- M0 Spike：[`docs/spike-ui.md`](docs/spike-ui.md)
- M0b：[`docs/m0b.md`](docs/m0b.md)
- M1：[`docs/m1.md`](docs/m1.md)
- 仓库：<https://github.com/madlaxcb/Glean>

## 功能

- RSS/Atom 订阅管理：添加、重命名、删除、编辑 URL、文件夹分组
- 条目右键菜单：标记已读/未读、星标、在浏览器中打开
- 侧栏未读数 badge：每个 feed 显示未读条目数
- 删除源时保留星标条目（非星标条目随源删除，星标条目保留）
- 全文抽取：readability 风格，自动/手动抽取原文
- 本地图片缓存：glean-img:// 自定义协议，离线可看
- Favicon 缓存：从 feed 或站点 HTML 自动发现并缓存
- 虚拟列表渲染 + FTS5 trigram 全文搜索（支持中文子串）
- 条件请求：ETag / Last-Modified，减少不必要传输
- 磁盘缓存：正文 HTML 写入本地文件，支持离线阅读
- 浅色/深色主题切换，阅读字体大小/行宽可调
- 键盘快捷键：j/k 换文、r 刷新、s 星标、t 切换主题
- OPML 导入/导出
- 全局/每源自定义刷新间隔，自动刷新
- HTTP/SOCKS5 代理支持
- 系统托盘（Windows）：最小化到托盘、托盘恢复、托盘刷新
- 自动更新检查（appcast.json）
- 单实例锁：防止多开

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
