# M0 UI Spike 记录

> 状态：**进行中（壳层交互基本可用）**  
> 路径：**A — egui + WebView2**  
> 最近确认构建：`448c479` 及之后 artifact

## 用户实测摘要

| 项 | 结果 | 备注 |
|----|------|------|
| 内容区显示 | **Pass** | WebView2 正文 |
| 外链系统浏览器 | **Pass** | ShellExecuteW |
| 最大化/还原 | **Pass** | |
| Stress 内存 | **观察 Pass** | 单实例 WebView，未线性暴涨 |
| 双击无 CMD | **Pass** | `windows_subsystem`；禁全局 SUBSYSTEM rustflags |
| 搜索可输入 | **Pass** | 顶栏 + 取消每帧 repaint |
| 点内容区后再点搜索 | **Pass** | 壳层点击 `SetFocus` 回主窗 |
| 左右分隔独立 | **Pass** | 不把 clamp 宽度写回 state |
| 标题栏随主题变暗 | **Partial** | DWM + SetTheme；部分 Win 仍白，不单独挡 M0 |
| 主题切换正文略慢 | 已知 | `load_html` 整页重载，Spike 可接受 |
| 中文 IME 候选框 | **待确认** | 顶栏搜索输入拼音时是否出现候选 |

## §9.0.2 门禁对照（摘要）

| 门禁方向 | 状态 |
|----------|------|
| 三栏 + 单实例换文 | Pass |
| Resize / 分隔条 | Pass |
| 最大化最小化 | Pass |
| 焦点：壳 ↔ WebView | Pass（搜索） |
| 外链 | Pass |
| 内存 Stress | 观察 Pass |
| IME | 待你确认候选框 |
| 标题栏暗色 | Partial（可接受） |
| H1 vs H2 真差异 | 仍弱（同源 child bounds） |

## 下一步（二选一）

1. **补测 IME**：顶栏搜索输入中文，看候选框是否出现 → 回填本表  
2. 若 IME 可接受（或可记风险继续）：**M0 记为有条件通过** → 开始 **M0b**（workspace 业务骨架，仍无完整订阅）  
3. 若 IME 硬 Fail 且不可接受 → 讨论是否切路径 B（Tauri）

> CI 绿 / 有 artifact **≠** 正式 M0 Pass；以本表为准。
