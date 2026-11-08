# 合并裁决笔记

## crates/codex-plus-core/src/diagnostic_log.rs

- 采用 upstream 的句柄复用、定时压缩、失败退避和 24 MB 日志上限。
- 保留本分支的内存诊断快照、清空联动与对应测试，日志行为两边功能齐全。

## crates/codex-plus-core/src/settings.rs

- 同时保留 `webSearch` 的持久化归一化与 upstream 的 `claudeAppExtensions` 设置。
- 保存时两边 `normalize()` 都执行，配置粒度互不替代。

## crates/codex-plus-core/src/lib.rs

- `web_search` 与 upstream 的 `whale` / `whale_full` 模块声明全部保留。

## crates/codex-plus-core/src/launcher.rs

- 同时保留 Windows 任务栏修复入口与 upstream 的 official mix 辅助函数；两者当前职责不同。
- 原生补丁状态、流转发测试 helper 和用量返回均用 upstream 新结构；本分支 web search 流 helper 继续调用其 updated 返回值。

## crates/codex-plus-core/src/protocol_proxy.rs

- `ChatSseToResponsesConverter` 同时保留 web search 的累计轮次用量和 upstream 的本次请求用量；`ChatSseState` 保留 `usage_baseline`、`turn_usage`、`upstream_usage` 三组状态。
- 上游请求调用采用 upstream 的 `header_timeout` 签名，web search 编排与 upstream 的 local compaction 记忆逻辑并存；web search 命中时提前返回，不走普通 compaction 记忆。

## apps/codex-plus-manager/src-tauri/src/commands.rs

- 采用 upstream 新增的 Claude 拓展开关命令；既有语音输入命令和返回 payload 不变。

## apps/codex-plus-manager/src/App.tsx

- 保留 web search 的前端归一化；upstream 已移除 `codexAppTypingEffect` 归一化字段与函数，跟随删除以免引用失配。
