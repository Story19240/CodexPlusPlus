# Codex++ Web Search 收尾提示（最终轮 reminder）

日期：2026-10-26
分支：`codex/web-search-upstream-merge`（v1.12.1 线，未提交）
改动文件：`crates/codex-plus-core/src/web_search.rs`、`crates/codex-plus-core/src/launcher.rs`
相关前置报告：《2026-10-19-CodexPlusPlus-WebSearch-Report.md》第 10.4 节

## 1. 解决什么问题

多轮 `web_search` 编排在最后一轮之后的 followup 有缺陷：`web_search` 工具声明被静默移除
（`retain_web_search_tool=false`），但没有任何东西告诉模型「搜索额度已用完，必须立即收尾」。

2026-10-20 凌晨 23:49 的实测就是这个缺陷的样本：`maxRounds=5` 跑满 5 轮后，最终响应里
模型输出一段「我再查……」的进度式文本、没有结构化工具调用、上游 `finish_reason=stop`，
Codex agent 于是判定本轮任务正常完成——用户看不到最终答案，观感上像断在半路。

同时暴露了一个协议事实：现有的超限文案（`web_search_round_limit` 的 error.message）
没有任何模型或用户读得到：

1. 该分支之后不再向上游发请求，模型当轮读不到；
2. 下一条用户消息时客户端回放历史，`append_responses_item` 只处理带 `content` 的项，
   `web_search_call` 没有对应分支、落到通配分支后整个 item 连 error 一起被丢弃；
3. `web_search.degraded` 诊断事件只记 `stage` / `searchRound` / `maxRounds`，不含文案；
4. 客户端界面如何渲染 failed 的 `web_search_call` 由上游 codex 决定，不在本仓库。

结论：靠「在降级文案上下功夫」改进体验是无效的，必须把话递到模型手里。

## 2. 设计取舍

目标语义经讨论后定为单一 `maxRounds` 闸门，不引入第二道上限：

- `maxRounds` 以内模型可自由搜满，每个 followup 都告知已用与剩余轮数，避免无度连搜或提前认输；
- 到达 `maxRounds` 时工具声明照旧移除，同时明确要求基于已有结果立即收尾、缺什么就说缺什么；
- 达到上限后模型若仍发起调用，沿用既有 item 级降级（failed item + 结果回填 + completed）。

落选方案及理由：

| 方案 | 不采用的原因 |
| --- | --- |
| 单独把 `search_round < max_rounds` 改成 `<=` | 有 off-by-one：`maxRounds=5` 会实际执行 6 轮；且它只让模型多一次「结构化表达未达标」的机会，该调用依然不执行、failure 文案依然无人读 |
| 新增硬上限（软预算 + hardLimit 双档） | 不需要第二道闸门，一个设置项已足够 |
| 让模型总结、由用户决定是否继续 | 等于把「是否做完」的判断交回用户，正是本次要消除的观感问题 |

## 3. 实现

### 3.1 `web_search.rs`

新增 `WebSearchFollowupNotice` 枚举（`web_search.rs:529`），两个变体对应两种预算状态：

```text
RoundsLeft { completed_rounds, max_rounds }   预算内：工具仍在，报进度
Exhausted  { completed_rounds, max_rounds }   预算用尽：工具已移除，必须收尾
```

`reminder_text()`（`web_search.rs:544`）生成英文正文（直接给上游模型读，不面向用户），节选：

```text
RoundsLeft:
  Internal reminder: web_search has completed {n} of {max} allowed rounds in this request
  ({max-n} remaining). If the results already answer the request, answer now and stop
  searching; only continue when specific information is still missing.

Exhausted:
  Internal reminder: web_search for this request has reached the configured limit of
  {max} rounds ({n} executed), and the web_search tool is no longer available.
  Do not announce another web search and do not request one. ... produce your final
  answer now. ... tell the user that searching can continue after their next message.
```

`build_web_search_followup_request_with_web_search_tool()` 增加 `notice: Option<WebSearchFollowupNotice>`
参数，在 tool 结果之后追加一条 `role: user` 消息（`web_search.rs:638`）。位置选择在 tool 结果
之后：不影响 `assistant.tool_calls` 与 `role: tool` 的配对，也不会夹在连续 tool 结果之间。
旧的 4 参调用方（非流式路径）传 `None`，行为不变。

### 3.2 `launcher.rs`

流式编排在每轮搜索后按预算状态构造提示（`launcher.rs:4016`）：

```rust
let followup_notice = if retain_web_search_tool {
    WebSearchFollowupNotice::RoundsLeft { completed_rounds: search_round, max_rounds }
} else {
    WebSearchFollowupNotice::Exhausted { completed_rounds: search_round, max_rounds }
};
```

提示只进上游 Chat 请求，客户端可见的响应流里不会出现（有断言看守）。

### 3.3 非流式路径的已知差异

`protocol_proxy.rs` 的非流式分支只执行 1 轮搜索后就移除工具，没有轮次循环，因此未注入提示。
该路径不在 Codex agent 的流式链路上，本次不改；若后续要补齐，需要先确认
「只跑一轮」是否应有独立的收尾文案。

## 4. 测试

新增：

```text
web_search::tests::followup_appends_internal_round_reminder
    预算内：提示为最后一条 user 消息、tool 配对不受影响、工具仍在；
    用尽：文案含 limit 与 final answer 要求、工具声明被移除
web_search::tests::followup_without_notice_keeps_tool_output_tail
    不传提示时消息尾部仍是 tool 结果（守卫旧行为）
```

更新既有用例的断言（提示追加后消息条数与尾部项发生变化）：

```text
launcher::proxy_stream_tests::web_search_live_stream_executes_sequential_rounds
    断言第 1/2 轮 followup 的进度提示文案，且客户端响应流不含 "Internal reminder"
launcher::proxy_stream_tests::web_search_round_limit_degrades_to_completed_response
    断言 maxRounds=1 的最终 followup 确实带上收尾提示
```

运行结果（v1.12.1 工作树）：

```powershell
cargo test -p codex-plus-core --lib -j1 web_search -- --test-threads=1               # 46 passed
cargo test -p codex-plus-core --lib -j1 -- --test-threads=1                         # 1223 passed / 4 ignored
cargo test -p codex-plus-core --test protocol_proxy -j1 -- --test-threads=1         # 206 passed
```

本次新增代码符合 rustfmt；仓库既有 drift 未一并处理。

## 5. 构建与启动

```powershell
cargo build --release -p codex-plus-manager   # 成功，产物 1.12.1
```

本机已有一个正式实例在跑（`D:\Software\CodexPlusPlus\codex-plus-plus-web.exe`），
它占用默认 guard 端口，新构建进程会被单实例守卫判为「已有实例」后交还焦点并退出。
因此本次用 `CODEX_PLUS_GUARD_PORT=64122` 独立启动：

```text
工作树：D:\Codex\codexplusplus\CodexPlusPlus-websearch-merge
产物：target\release\codex-plus-plus.exe（18:35 构建）
pid 21580，监听 127.0.0.1:64122，日志 manager.start version=1.12.1
```

注意：桌面 App 的 `/v1/responses` 流量仍由旧实例（pid 13996）服务，
要实测本次改动需要把旧实例换成新实例，这一步会影响进行中的会话，未擅自执行。

## 6. 过程中的失误

首次实现直接落在了本地 `main` 分支——那是 1.10.0 时代的旧 fork 线，落后 `origin/main` 67 个提交，
既不是主线也不是承载 Web Search 的线。发现后用 `git diff` 生成补丁、归一化行尾后
`git apply` 到 v1.12.1 工作树重新落地，并在此完成全部测试与构建。

## 7. 遗留事项

1. 非流式路径（单轮）无收尾提示，见 3.3；
2. 达到上限后的 failed item 文案仍无人读取，保留作 rollout 排障用；
3. 若要「未达标则持续工作到目标」，需要独立的自动续跑机制，会改变 `maxRounds` 语义，
   不在本次范围内。
