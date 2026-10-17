# Web Search 交叉验证 Bug 报告

日期：2026-10-17

仓库：`D:\Codex\codexplusplus\CodexPlusPlus`

分支状态：`main`，本地领先 `origin/main` 22 个提交。

关联提交：

- `b996d980 fix(web-search): support sequential search rounds`
- `3e3a7385 fix(web-search): preserve mixed client tool calls`

本报告基于以下材料交叉验证：

- `D:/Codex/codexplusplus/web-search-cross-validation-2026-10-16.md`
- `docs/reports/2026-10-12-web-search-cross-validation-handover.md`
- `docs/reports/2026-10-12-web-search-verification.md`
- 本地诊断日志：`C:/Users/22143/.codex-session-delete/codex-plus.log`
- 本地运行实例：`http://127.0.0.1:57321`

日志与配置检查过程中未读取、未复制、未输出任何 API Key、`auth.json` 或 `config.toml` 凭据。

## 1. 结论摘要

| 编号 | 问题 | 等级 | 结论 |
| --- | --- | --- | --- |
| WS-2026-10-17-01 | 同一个响应内第 2 次 `web_search` 不执行，变成文本工具调用 | P1 | 已修复并有测试锁定 |
| WS-2026-10-17-02 | `web_search` 与其它客户端工具同轮混合调用导致 `response.failed` / 断连 | P1 | 已修复并有测试锁定 |
| WS-2026-10-17-03 | 长上下文请求中模型不发起 `web_search`，表现为“搜索中断” | P2 | 非 Web Search 代码缺陷，需修正上下文配置或压缩会话 |
| WS-2026-10-17-04 | 增强总开关优先级、多查询执行、单查询真实链路 | P2 | 交叉验证通过，未见回归 |

## 2. WS-2026-10-17-01：同一响应内第二次搜索不执行

### 2.1 现象

用户反馈“只能搜索一次，第二次突然中断”。定向复现请求要求模型在一个响应中先后执行两次搜索：先搜 Rust 官网，拿到结果后再搜 Go 官网。

修复前真实响应表现为：

1. 第一次 `web_search_call` 正常 completed。
2. Exa 第一次返回 HTTP 200，5 条结果。
3. 二段模型明确推理“现在需要搜 Go”，但最终没有第二次 `web_search_call`。
4. 模型把第二次工具调用以 `<tool_call>web_search...</tool_call>` 文本形式写进 `output_text`。
5. 请求最终 HTTP 200 并 `response.completed`，没有 `response.failed`，因此容易表现为“静默中断”。

### 2.2 根因

`crates/codex-plus-core/src/web_search.rs` 的 `build_web_search_followup_request()` 在构建二段模型请求时移除了 `web_search` 工具。原意是避免模型连续发起未编排的搜索调用，但这直接导致“同一响应内多轮搜索”不可能发生。第一轮搜索后的二段模型即使再返回 `web_search` 调用，也只是未执行调用，最终被转换成普通文本输出。

### 2.3 修复方式

提交 `b996d980` 引入有上限的连续搜索编排：

- 新增 `build_web_search_followup_request_with_web_search_tool()`。
- 流式路径按搜索轮数保留二段 `web_search` 工具。
- 同一响应内最多执行 `MAX_WEB_SEARCH_ROUNDS = 3` 轮搜索。
- `ChatSseToResponsesConverter::resume_after_web_search()` 延迟二段后的 `response.completed`，等待判断是否还有新的搜索调用。
- 每轮记录 `searchRound`、`queryCount`、`queryLengths`、`retainWebSearchTool`。
- 达到轮数上限后最后一次二段请求移除 `web_search`，并显式返回 `web_search_round_limit`，不再无限循环。
- 若模型在连续搜索之间先输出 reasoning / text，进入下一轮前先 `pause_for_web_search()`，避免客户端看到悬空的中间文本流。

### 2.4 验证

新增/更新测试：

- `web_search_live_stream_executes_sequential_rounds`
- `web_search_live_stream_preserves_reasoning_and_merges_followup`
- `followup_can_retain_web_search_for_another_round`

真实链接路验证：

- 第一次搜索：Exa HTTP 200，5 条结果。
- 第二次搜索：Exa HTTP 200，5 条结果。
- 最终输出包含两次 `web_search_call`，均为 completed。
- SSE 正常输出 `response.completed` 与 `data: [DONE]`。

定向测试通过：

```text
cargo test -p codex-plus-core --lib web_search -- --test-threads=1
36 passed / 0 failed
```

## 3. WS-2026-10-17-02：混合工具调用导致失败

### 3.1 现象

用户反馈：

```text
正在重新连接 /5
stream disconnected before completion: 服务端 Web 搜索暂不支持与其它工具在同轮混合调用
```

日志中的关键失败证据：

```json
{"event":"web_search.failed","detail":{"stage":"parse_first_response","argumentsValid":false}}
{"event":"helper.protocol_proxy_stream_failed","detail":{"path":"/v1/responses","status":"200 OK","stream_outcome":"failed","wireApi":"ChatCompletions","webSearch":true,"error":"服务端 Web 搜索暂不支持与其它工具在同轮混合调用"}}
```

这不是网络断开，而是响应已经开始后由代理主动写入 `response.failed`，客户端随后表现为断连重连。

### 3.2 复现

构造真实请求，同时提供 `web_search` 与 `other_tool`，并明确要求模型在同一条 assistant 消息里调用两个工具。修复前上游确实可能一次返回 `web_search_call` 和另一个客户端工具调用，此时旧解析器命中保护分支并直接失败。

### 3.3 根因

原有设计假设“服务端 Web Search 不能与客户端工具混在同一轮”。但 Codex 正常请求会同时挂载大量客户端工具；某些模型在并行工具调用或长上下文中会同时返回 `web_search` 和其它工具。相关失败日志显示该轮 `toolCount` 达到 54。

另外，仅设置 `parallel_tool_calls=false` 并不足够：本地真实诱导测试中，上游虽然收到 `parallel_tool_calls=false`，仍然可能同轮返回两个工具调用。因此不能只依赖参数抑制，必须让代理层支持这种返回。

### 3.4 修复方式

提交 `3e3a7385` 将混合调用从“硬失败”改为“可执行编排”：

- [web_search.rs](D:/Codex/codexplusplus/CodexPlusPlus/crates/codex-plus-core/src/web_search.rs:58)：新增 `web_search_calls_with_client_tools()`，提取所有 `web_search` 调用并返回是否混有客户端工具。
- [launcher.rs](D:/Codex/codexplusplus/CodexPlusPlus/crates/codex-plus-core/src/launcher.rs:3193)：第一轮使用可识别混合调用的解析。
- [launcher.rs](D:/Codex/codexplusplus/CodexPlusPlus/crates/codex-plus-core/src/launcher.rs:3289)：混合调用时执行 `web_search`，并把其它工具调用交还 Codex 客户端。
- [protocol_proxy.rs](D:/Codex/codexplusplus/CodexPlusPlus/crates/codex-plus-core/src/protocol_proxy.rs:1767)：新增 `push_web_search_results()`，把搜索结果落成普通 assistant message。
- [launcher.rs](D:/Codex/codexplusplus/CodexPlusPlus/crates/codex-plus-core/src/launcher.rs:3509)：若模型是“先搜索成功，二段再调用客户端工具”，也先累积搜索结果，再交给客户端执行。

后续 Codex 请求历史中因此可以同时包含：

1. `web_search_call`
2. 客户端工具调用
3. 客户端工具输出
4. 一条包含 Web Search 结果的 assistant message

辅助抑制措施：

- `responses_to_chat_completions_with_web_search()` 强制 `parallel_tool_calls=false`。
- `web_search_chat_tool()` 工具描述明确要求单独调用 `web_search`。

### 3.5 验证

新增测试：

- `web_search_calls_streaming_can_extract_mixed_client_and_server_tools`
- `web_search_live_stream_mixed_tools_returns_client_tool_and_results`
- `web_search_live_stream_returns_client_tool_after_search_with_results`

集成测试覆盖两种情况：

1. 上游同轮同时返回 `web_search` 与 `apply_patch`。
2. 上游先返回 `web_search`，二段再返回 `apply_patch`。

两者都验证：

- `web_search` 真实执行并 completed。
- 客户端工具调用仍会返回，不被丢弃。
- 搜索结果写入响应历史。
- 只有一个 `response.completed`，没有 `response.failed`。

真实本地复测：

```text
web_search.provider_status: Exa 200
web_search.result_count: 5
web_search.client_tool_after_search_passthrough: searchResultCount=1
helper.protocol_proxy_stream_ok: 200 OK
```

最终 SSE 包含 `response.completed` 与 `data: [DONE]`，不再出现“暂不支持与其它工具在同轮混合调用”。

## 4. WS-2026-10-17-03：长上下文导致模型不搜索

### 4.1 现象

最新若干轮真实请求表现为模型不再调用 `web_search`，日志只有：

```json
{"event":"web_search.no_call_passthrough","detail":{"callCount":0}}
```

同时上游请求 HTTP 200，最终流也标记成功。也就是说代理没有失败，而是模型没有发起搜索。

### 4.2 关键证据

该轮请求体达到 1,192,298 bytes，并出现：

```json
{"event":"vlm_context_overflow","detail":{"context_window":272000,"text_only_estimated_tokens":553383,"skipped_images":0}}
```

后续又出现 `text_only_estimated_tokens` 增长到 581436 的记录，而运行时仍按 `context_window=272000` 处理。纯文本估算 token 已达模型窗口的两倍以上，此状态下模型不调用工具、提前结束或输出异常都不属于 Web Search 执行层失败。

### 4.3 配置线索

仓库内置的 `assets/glm-model-metadata.json` 中 `glm-5.3` 元数据是 `1M` 上下文，但运行时该请求按 `272000` 处理。这说明当前活动 profile 很可能用全局 `contextWindow=272000` 覆盖了内置元数据，或者 1M catalog 未对该请求生效。

### 4.4 处置建议

这不是本次需要改代码的 Web Search Bug，但会直接影响用户感知：

1. 如果该供应商的 `z-ai/glm-5.3` 确实支持 1M，将 model_list 改为 `z-ai/glm-5.3[1M]`。
2. 或者清空当前 profile 中覆盖内置元数据的全局 `contextWindow` / auto-compact 配置。
3. 当前会话先手动压缩或新开会话，再验证 Web Search 工具调用。

## 5. WS-2026-10-17-04：交叉验证通过项

以下既有修复在交叉验证中未发现回归：

| 验证项 | 结果 |
| --- | --- |
| 增强总开关优先于 Web Search 子开关 | 通过，测试 `web_search_master_switch_takes_precedence_over_sub_switch` |
| Web Search 子开关保留配置但不生效 | 通过 |
| 多查询解析、去重、上限 10 条 | 通过，测试 `web_search_queries_are_deduplicated_and_capped` |
| 同一 call 多查询全部执行并合并 | 通过，测试 `web_search_calls_execute_every_query_and_merge_results` |
| 单条查询失败不影响其它查询 | 通过，测试 `web_search_calls_allow_partial_failures` |
| 超过并发上限时仍可执行 | 通过，测试 `web_search_calls_supports_more_than_concurrency_limit` |
| 单查询真实链路 Exa -> 二段模型 -> SSE | 通过 |

单查询真实链路证据：

```text
callCount=1
queryCount=1
queryLengths=[42]
Exa statusCode=200
resultCount=5
followup statusCode=200
response.completed + [DONE]
```

## 6. 构建、运行与测试记录

### 6.1 测试

```text
cargo test -p codex-plus-core --lib web_search -- --test-threads=1
36 passed / 0 failed / 0 ignored
```

```text
cargo test -p codex-plus-core --lib -- --test-threads=1
914 passed / 0 failed / 3 ignored
```

既有 MSVC `LNK4098` 警告仍然存在，属于非阻断构建警告。

### 6.2 构建与启动

使用仓库外本地脚本：

```powershell
D:\Codex\codexplusplus\build-local.bat
```

脚本行为已检查：仅执行前端 `vite:build`、Rust release 构建、替换本地 release exe、停止旧实例并启动新实例，然后等待 `127.0.0.1:57321` 被监听。

最终 release 产物时间：

```text
D:\Codex\codexplusplus\CodexPlusPlus\target\release\codex-plus-plus.exe
LastWriteTime: 2026-10-16 20:50:44
```

写这份报告时观察到本地实例：

```text
PID 20748
D:\Codex\codexplusplus\CodexPlusPlus\target\codexplusplus-local-build\release\codex-plus-plus.exe
127.0.0.1:57321 Listen
```

未运行未知脚本，未安装额外依赖，未读取或输出 API Key。

### 6.3 工作区状态

```text
## main...origin/main [ahead 22]
?? verify_patch_test.txt
```

`verify_patch_test.txt` 是交接文档中要求保留但不提交的未跟踪文件，本报告未将其纳入提交。

## 7. 剩余风险与后续建议

1. `parallel_tool_calls=false` 只是抑制手段，部分上游可能忽略；代理层现已支持混合调用，这是最终安全网。
2. 连续搜索上限是 3 轮。对于需要 3 轮以上搜索的任务，会显式返回 `web_search_round_limit`；是否需要提高上限应作为产品决策单独确认。
3. 搜索结果作为 assistant message 写入历史可保证下一轮不丢，但也会增加上下文体积；超大上下文会话需要配合 1M 配置或压缩机制。
4. 当前 bug 修复已聚焦 Rust 核心库，未改动前端行为；前端生产构建已通过，但未重跑前端全量单测。
