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

## 构建修复

- `protocol_proxy.rs::extension_rejected_response`：补 `UpstreamProxyResponse.web_search_context: None`。结构体已含该字段，其余同仓初始化均按 `None` 填充。
- `protocol_proxy.rs` 的 3 处 `send_responses_upstream_request` 调用（web search 编排 / 摘要 / 二段请求）补第 10 个参数 `upstream_stream_header_timeout()`，与 upstream 新签名 `header_timeout: Duration` 对齐。
- `launcher.rs` 测试与 `launcher/final_response_tests.rs` 的 `UpstreamProxyResponse` 初始化补 `web_search_context: None`，保持全仓字段齐全。
- `protocol_proxy.rs` / `launcher.rs` 中显式构造 `ProxySessionHeaders` 时补齐 `client_identity`；测试点位按 `&[]` 填充，生产点位沿用已有请求头变量。全仓扫描确认没有其它显式初始化缺该字段。
- `protocol_proxy.rs` 的 4 处 `upstream_request_parts` 测试调用补 `web_search_execution: false`，与函数签名 8 个参数对齐；非 web search 测试不涉及新行为。
- `watcher.rs` 的 `macos_codex_process_ids_with` 加 `cfg(any(target_os = "macos", test))`：测试代码在非 macOS 也要编译，函数本身无改名；`macos_codex_process_ids_without_debug_port_with` 已是同款 cfg，因此测试可调用。
- `apps/codex-plus-manager/src-tauri/src/commands.rs`：合并后非测试的 `run_codex_multi_worker` 已改用 `multi_worker` 的新生命周期逻辑，`MultiWorkerExitGrace` 定义随旧实现一起丢失，但三个测试仍引用它。按历史提交 7886f69c 恢复该类型，并放入 `mod tests` 仅供测试使用；未改业务逻辑。
