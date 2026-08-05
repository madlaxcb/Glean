# [OPEN] 内存观测日志缺失

## 症状

Windows 运行日志包含配置、条目和文章渲染记录，但没有 `[memory-debug]`。

## 假设

1. 运行的不是包含观测代码的最新 EXE。
2. 日志写入了与用户查看不同的可执行文件目录。
3. 10 秒定时观测未获得足够的 UI 更新周期。
4. 运行的是安装目录中的旧 EXE，而不是新下载的 Artifact。

## 当前证据

- GitHub Actions Run 167 的提交为 `be69d2c1a9f2fead9c91e919fd89dfd5600a114f`。
- Run 167 成功生成 Windows EXE 和安装器。
- 用户日志包含 `entries=27457`，但没有 `[memory-debug]`。

## 状态

等待加入启动标记和路径标记后的 Windows 运行证据。
