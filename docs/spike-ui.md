# M0 UI Spike 记录

> 状态：**有条件通过（Conditional Pass）**  
> 路径：**A — egui + WebView2**  
> 用户确认日：2026-07-28（IME 候选框）  
> 参考构建：`448c479` 及之后（焦点修复）

## 结论

路径 A（egui 壳 + WebView2 阅读区）**可作为产品主路径继续**。  
**M0 Spike 目标达成**；已知债不挡进入 **M0b**（业务骨架），但须在里程碑中跟踪。

CI 绿 / artifact **仍 ≠** 产品完成；仅表示 Hybrid 集成风险已降到可接受。

## 用户实测摘要

| 项 | 结果 | 备注 |
|----|------|------|
| 内容区显示 | **Pass** | WebView2 正文 |
| 外链系统浏览器 | **Pass** | ShellExecuteW |
| 最大化/还原 | **Pass** | |
| Stress 内存 | **观察 Pass** | 单实例 WebView |
| 双击无 CMD | **Pass** | `windows_subsystem`；禁全局 SUBSYSTEM rustflags |
| 搜索可输入 | **Pass** | 顶栏 + 取消每帧 repaint |
| 点内容区后再点搜索 | **Pass** | 壳层 `SetFocus` 回主窗 |
| 左右分隔独立 | **Pass** | 不写回 clamp 宽度 |
| 中文 IME 候选框 | **Pass** | 用户确认顶栏搜索正常 |
| 标题栏随主题变暗 | **Partial** | DWM + SetTheme；部分 Win 仍白，**不挡 M0** |
| 主题切换正文略慢 | 已知 | `load_html` 整页重载 |
| H1 vs H2 真差异 | **债** | 当前同源 child bounds，标签切换为主 |

## §9.0.2 门禁

| 门禁 | 状态 |
|------|------|
| 三栏 + 单实例换文 | Pass |
| Resize / 分隔条 | Pass |
| 最大化最小化 | Pass |
| 焦点壳 ↔ WebView | Pass |
| 外链 | Pass |
| 内存 Stress | 观察 Pass |
| IME | **Pass** |
| 标题栏暗色 | Partial（可接受） |
| H1/H2 对比 | 未真正分叉（记录为债） |

## 已知技术债（进 M0b 前知晓即可）

1. **H2 Overlay** 未实现独立顶层跟随窗，与 H1 行为基本相同  
2. **标题栏暗色** 在部分 Windows 主题下无效  
3. **主题切换** 整页 `load_html`，有一帧延迟  
4. 父 HWND 仍依赖 EnumWindows 标题匹配（可用 eframe raw handle 加固）

## 下一步

**M0b**：`glean-core` 领域扩展 + SQLite 骨架 + Command/Event 接线（仍无完整订阅抓取也可先落库模型）。  
**M1**：垂直切片（真 RSS 源可读）。

决策：**不因 Partial 标题栏 / H2 债切换 Tauri。**
