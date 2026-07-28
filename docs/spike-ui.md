# M0 UI Spike 记录

> 状态：**进行中**  
> 路径：**A — egui + WebView2**

## 用户实测摘要

| 项 | 结果 | 备注 |
|----|------|------|
| 内容区显示 | Pass | |
| 外链系统浏览器 | Pass | |
| 最大化/还原 | Pass | |
| Stress 内存 | 观察 Pass | |
| 双击弹 CMD | Fail→修 | `#![windows_subsystem="windows"]` + CI PE Subsystem=2；**禁止**全局 rustflags（会弄坏 proc-macro DLL） |
| 搜索无法输入 | Fail→修 | 移到顶栏；取消每帧 `request_repaint` |
| 左分隔带动右 | Fail→修 | 禁止把 clamp 后的宽度写回 state |
| 标题栏不暗 | Partial | DWM attr 19/20；部分主题/Win 版本仍可能无效 |
| 主题切换正文略慢 | 已知 | `load_html` 整页重载，Spike 可接受 |
| IME 候选框 | 风险 | 顶栏搜索再测 |

## 再测清单（新 artifact）

1. 双击 **无黑 CMD**  
2. 顶栏「搜索」可点入、可输入  
3. 只拖左分隔：右分隔位置可变（阅读区变），但列表**目标宽度**不被动永久缩小  
4. 只拖右分隔：导航宽度不变  
5. Theme：正文变暗；标题栏尽量变暗  
6. 点 Article 1 外链：浏览器打开且尽量不闪 CMD  
