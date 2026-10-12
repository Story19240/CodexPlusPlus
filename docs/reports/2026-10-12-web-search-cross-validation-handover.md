# Web Search 交叉验证交接文档

日期：2026-10-12
分支：`codex/add-web-search`
当前 HEAD：`f64ba99 docs: verify web search implementation`
状态：**当前实现未达验收，不能视为可用版本**

## 1. 结论

用户反馈的三个核心症状需要分开定性：

1. **搜索请求一直没有发起，但前端不报错**：目前无法从日志确认，因为缺少 Web Search 专属诊断事件。
2. **输出不像流式**：代码已确认，第一轮上游响应会被完整读取后才判断是否出现 `web_search` 调用；在此之前不会向 Codex 客户端下发任何增量内容。
3. **第一轮没有思考过程，直接出结果**：代码已确认。进入搜索分支后，第一轮 reasoning / 文本 / 工具调用不会合并进最终客户端响应，最终只返回二段模型响应。

因此，当前问题不是单纯的 Exa / Tavily 请求问题，而是服务端编排模型本身存在缺陷：

```text
先完整吞掉第一轮响应
-> 判断是否搜索
-> 执行搜索
-> 发起二段模型请求
-> 只把二段结果返回给客户端
```

这会必然造成长时间无输出、无第一轮思考过程，以及大上下文下更容易重连。

## 2. 交叉验证范围

只验证当前分支这三个提交：

```text
f64ba99 docs: verify web search implementation
7782c8b fix(protocol): forward buffered web search streams
8b60569 feat(protocol): execute web search through Exa or Tavily
```

重点文件：

```text
crates/codex-plus-core/src/protocol_proxy.rs
crates/codex-plus-core/src/launcher.rs
crates/codex-plus-core/src/web_search.rs
crates/codex-plus-core/src/settings.rs
apps/codex-plus-manager/src/App.tsx
apps/codex-plus-manager/src/web-search-settings.ts
```

不要扩大成全仓库审计，也不要把旧 handover 里的实现当成当前实现。当前实现是重新做的。

## 3. 已确认的代码问题

### 3.1 第一轮响应被完整缓冲

位置：[protocol_proxy.rs](<D:/Codex/codexplusplus/CodexPlusPlus/crates/codex-plus-core/src/protocol_proxy.rs:2419>)

关键逻辑：

```rust
let first_body = upstream.read_body().await?;
```

执行顺序是：

1. 上游返回 HTTP 200 响应头
2. Codex++ 读取完整第一轮 Chat SSE / JSON 响应体
3. 解析是否存在 `web_search` tool call
4. 读取完成后才决定无搜索直通，或进入 Exa / Tavily 编排

在此期间，helper 不会向 Codex 客户端下发任何第一轮增量数据，也没有 keepalive。

### 3.2 无搜索调用分支也不是流式

位置：[protocol_proxy.rs](<D:/Codex/codexplusplus/CodexPlusPlus/crates/codex-plus-core/src/protocol_proxy.rs:2631>)

当第一轮没有 `web_search` 调用时，代码把完整 Chat 响应转换成 Responses SSE，并放入 `buffered_body`。

launcher 的新分支会一次性写出：

```text
HTTP 200 + Content-Type: text/event-stream + 完整 SSE body
```

位置：[launcher.rs](<D:/Codex/codexplusplus/CodexPlusPlus/crates/codex-plus-core/src/launcher.rs:2007>)

所以即使未触发搜索，只要请求声明原生 `web_search` 且功能开启，第一轮也会被完整缓冲后一次性下发。

`7782c8b` 只修复了 `take_response()` 找不到 live response 导致连接直接断开的问题。它没有恢复流式输出。

### 3.3 搜索分支丢弃第一轮可见输出

位置：[web_search.rs](<D:/Codex/codexplusplus/CodexPlusPlus/crates/codex-plus-core/src/web_search.rs:370>)

当前二段请求会追加：

```text
assistant message + tool_calls
role=tool 搜索结果
```

但最终客户端响应只来自二段模型响应。

以下第一轮内容不会下发给客户端：

```text
reasoning_content
普通文本输出
web_search_call 可见事件
```

因此“第一轮没有思考过程，直接出结果”是当前实现的确定行为，不是上游模型没有思考。

### 3.4 二段请求仍可能再次调用 web_search

`build_web_search_followup_request()` 复制第一轮 Chat 请求，并保留 `tools`。

因此二段模型仍可能再次调用 `web_search`。当前实现没有二段递归编排；如果二段再次调用搜索，现有转换可能把它当作未执行的 web search 输出并生成失败响应。

这是一个高风险场景，目前没有测试覆盖。

### 3.5 JSON 转 SSE 的合成事件过少

位置：[protocol_proxy.rs](<D:/Codex/codexplusplus/CodexPlusPlus/crates/codex-plus-core/src/protocol_proxy.rs:2704>)

`responses_json_as_sse()` 只合成：

```text
response.completed
data: [DONE]
```

没有合成：

```text
response.created
response.output_item.added
response.output_text.delta
response.output_item.done
```

这用于“客户端请求流式，但上游返回 JSON”的场景。真实 Codex 客户端是否稳定接受只有 `response.completed` 的流尚未验证。

## 4. 现场日志证据

当前日志只有以下通用事件：

```text
helper.request
protocol_proxy.upstream_request
protocol_proxy.upstream_response
helper.protocol_proxy_stream_ok
```

近期请求时间线：

| 请求体大小 | 上游 200 距请求开始 | `stream_ok` 距请求开始 |
|---:|---:|---:|
| 1,950,390 bytes | 42.2 秒 | 194.3 秒 |
| 1,966,826 bytes | 7.3 秒 | 未见对应完成事件，64 秒后出现下一请求 |
| 1,967,160 bytes | 12.0 秒 | 45.2 秒 |

这与代码行为一致：第一轮完整读取和搜索编排期间，下游长时间无数据。

但这些日志不能证明 Exa / Tavily 是否实际被调用，因为缺少 Web Search 专属事件。

## 5. “请求不到但不报错”的待验证原因

当前有三种可能，必须用日志区分：

1. 模型第一轮没有调用 `web_search`，因此没有 Exa / Tavily 请求
2. 模型调用了工具，但供应商返回的字段形态无法被 `parse_chat_response()` 解析
3. 搜索请求发起了，但当前没有日志，无法观察

现有代码只在以下条件全部满足时才会搜索：

```text
settings.web_search.enabled = true
relay.protocol = chatCompletions
request_has_native_web_search(request) = true
第一轮响应解析出 web_search tool call
```

## 6. 必须补的诊断事件

继续真实链路排查前，先补以下事件。不得记录 API Key、完整搜索词或完整用户上下文。

```text
web_search.enabled
web_search.native_tool_detected
web_search.first_response_received
web_search.call_detected
web_search.call_count
web_search.provider_request
web_search.provider_status
web_search.result_count
web_search.followup_request
web_search.followup_response
web_search.no_call_passthrough
web_search.failed
```

建议字段：

```text
provider
query_length
result_limit
timeout_seconds
call_id
arguments_valid
status_code
result_count
```

## 7. 缺失的关键测试

### 7.1 第一轮无搜索调用时的流式行为

构造上游逐步发送 reasoning delta / text delta，断言下游在上游 EOF 前收到增量事件。

当前实现预期失败。

### 7.2 第一轮 reasoning 后调用 web_search

构造上游：

```text
reasoning delta
web_search tool call
[DONE]
```

断言：

```text
客户端能看到第一轮 reasoning
客户端能看到搜索过程或等价状态
二段结果继续流式返回
```

当前实现预期失败。

### 7.3 二段再次调用 web_search

构造第一轮搜索后，二段再次调用 `web_search`。当前没有产品决策，也没有测试。

可选策略：

```text
显式失败
或再次执行搜索
或二段移除 web_search 工具，禁止重复调用
```

### 7.4 长等待 keepalive

断言第一轮读取、搜索请求、二段等待期间，下游不能长时间无字节。

当前实现预期失败。

### 7.5 JSON 上游但客户端请求 SSE

验证只发送 `response.completed` 是否满足真实 Codex 客户端协议。

## 8. 下一轮处理顺序

1. 先补 `web_search.*` 诊断事件，不要继续盲测
2. 补第一轮流式、reasoning 保留、二段重复搜索、keepalive 的失败测试
3. 重新设计第一轮处理，不能继续使用简单 `read_body()` 全量缓冲
4. 明确客户端可见协议：第一轮 reasoning 是否显示、搜索过程是否显示、二段输出如何衔接
5. 协议行为定住后，再做真实 Exa / Tavily 链路

## 9. 第一轮思考保留方案

### 9.1 目标行为

保留服务端 Web Search 的同时，第一轮思考必须实时下发给 Codex。目标时间线如下：

```text
Codex 请求 stream=true
-> helper 立即返回 SSE 响应头
-> 第一轮 Chat 上游开始流式返回
-> reasoning delta 实时转换并下发给 Codex
-> 如果第一轮没有 web_search：
   按原 Chat -> Responses 流式转换继续，正常 response.completed
-> 如果第一轮出现 web_search tool call：
   1. 已经出现的 reasoning / text 继续保留在同一个 SSE 流中
   2. 下发服务端执行的 web_search_call 状态
   3. SSE 不结束
   4. 执行 Exa / Tavily
   5. 把搜索结果追加进二段模型请求
   6. 二段响应继续流式下发
   7. 最终只发送一个 response.completed
```

验收重点是：**第一轮 reasoning 必须在上游 EOF 前到达 Codex**。搜索结束后一次性补发 reasoning 只能算内容保留，不算流式体验合格。

### 9.2 必须保留的第一轮输出

进入搜索分支后，以下第一轮内容不能丢弃：

```text
reasoning_content
已出现的普通文本 content
web_search tool call 的可见状态
```

推荐处理规则：

- reasoning delta：边收到边转换为 Responses reasoning 事件
- text delta：边收到边转换为 Responses text 事件
- tool call arguments：必须缓冲到完整，因为只有完整 JSON 才能确定 query
- tool call 完整后：执行搜索，并给客户端一个明确的服务端执行状态
- 二段 reasoning / text：继续追加输出，不得覆盖第一轮 item

### 9.3 编排位置必须调整

当前 `execute_web_search_round()` 位于 `protocol_proxy`，但它没有 Codex TCP stream 的写入权，因此只能先 `read_body()` 全量缓冲。这是架构问题。

推荐把第一轮 Web Search 编排移动到 launcher 层，或让 `protocol_proxy` 返回一个可由 launcher 执行的流式编排计划。

目标职责划分：

```text
protocol_proxy
-> 选择 relay
-> 生成第一轮 Chat 请求
-> 返回 live upstream response + relay/request context

launcher
-> 立即写 SSE 响应头
-> 增量解析第一轮 Chat SSE
-> 实时下发 reasoning / text
-> 检测 web_search tool call
-> 执行搜索
-> 发起二段请求
-> 续接 output_index
-> 合并最终 response.completed
```

不允许继续使用这个模式作为最终方案：

```rust
let first_body = upstream.read_body().await?;
```

它最多只能作为短期兼容分支，不能作为 Web Search 的验收实现。

### 9.4 SSE 续接契约

第一轮和二段必须在同一个 Responses SSE 流中续接，不能简单拼接两个完整响应。

必须满足：

```text
只有一个 response.created
只有一个 response.completed
output_index 单调递增且不重复
item id 不冲突
最终 response.completed.response.output 包含完整输出
```

最终 `output` 至少应包含：

```text
第一轮 reasoning
第一轮已出现文本（如存在）
web_search_call
二段 reasoning（如存在）
二段最终 message
```

示例结构：

```text
output_index 0: 第一轮 reasoning
output_index 1: 第一轮文本（如存在）
output_index 2: web_search_call
output_index 3: 二段 reasoning（如存在）
output_index 4: 二段 message
```

二段转换器不能从 `output_index=0` 重新开始。它必须继承第一轮转换器的：

```text
next_output_index
已完成的 output items
response id
model
usage 汇总规则
```

### 9.5 搜索等待期间的 keepalive

从检测到 tool call 到二段响应开始输出之间，下游不能长时间无字节。

至少覆盖三个等待阶段：

```text
Exa / Tavily 请求
二段模型响应头
二段首个增量事件
```

keepalive 必须使用 Codex 客户端已确认可接受的事件；实现前先用协议测试确认，不允许发明未知事件类型。

如果搜索或二段失败，且第一轮内容已经下发，不能把已完成的 reasoning / text item 改成失败。应保持已完成 item，然后追加一个显式失败事件或失败 item。

### 9.6 需要新增的流式状态

第一轮转换器需要新增状态，而不是只有最终 JSON：

```text
reasoning item 是否已开始
reasoning item 是否已结束
text item 是否已开始
text item 是否已结束
web_search tool call 是否完整
web_search call_id
web_search arguments
下一个 output_index
已生成的 output items
是否已经发送 response.created
是否已经发送 response.completed
```

这些状态要能传递给二段转换器，用于续接同一个 SSE 流。

### 9.7 方案级别

可以分两步落地，但验收必须以第二步为准。

#### 第一步：内容保留（不满足最终验收）

在当前全量缓冲实现中，把第一轮 reasoning / text / web_search_call 合并进最终 `response.completed.output`。

效果：

```text
用户最终能看到第一轮思考
```

仍然不合格：

```text
不是流式
长时间无输出
大上下文下仍可能重连
```

#### 第二步：真流式编排（验收目标）

第一轮 reasoning / text 实时下发，搜索和二段续接同一个 SSE 流。

验收必须做到：

```text
第一轮 reasoning 在上游 EOF 前到达 Codex
搜索等待期间有 keepalive
二段输出继续流式
最终只有一个 response.completed
最终 output 合并第一轮与二段内容
```

### 9.8 必须新增的测试

#### 测试 A：reasoning 后触发搜索

第一轮 mock SSE：

```text
reasoning delta
web_search tool call
[DONE]
```

下游断言顺序：

```text
第一轮 reasoning delta
web_search_call 状态
二段 answer delta
response.completed
```

并且断言 reasoning 事件在第一轮 mock EOF 前已经写出。

#### 测试 B：reasoning 后不触发搜索

第一轮 mock SSE：

```text
reasoning delta
text delta
[DONE]
```

下游断言：

```text
reasoning / text 均为实时增量
不调用 Exa / Tavily
只有一个 response.completed
```

#### 测试 C：output_index 续接

构造第一轮两个输出 item，再让二段返回 message，断言：

```text
第一轮 item 使用 index 0/1
二段 message 使用 index 2
最终 completed.output 顺序完整
```

#### 测试 D：部分输出后搜索失败

第一轮 reasoning 已下发后，让 Exa / Tavily 失败。

断言：

```text
已下发 reasoning 不被撤回
后续有显式失败事件
SSE 正常关闭
客户端不需要重连才能知道失败
```

## 10. 当前测试的有效边界

已通过：

```text
cargo test -p codex-plus-core --lib web_search
14 passed
```

它只能证明：

- Exa / Tavily 请求与响应解析可用
- 标准 tool call 可解析
- 二段请求包含 tool result
- 二段请求 `stream=true`
- 设置持久化可用
- buffered response 不再直接断开

不能证明：

- 第一轮是流式
- 第一轮 reasoning 可见
- 长等待不重连
- 二段不会再次搜索
- 真实 Exa / Tavily 可用
- 真实 Codex 客户端体验正常

## 11. 最终结论

当前版本的核心缺陷是服务端编排模型：

```text
完整吞掉第一轮响应后再判断是否搜索，并把最终可见输出压缩成二段响应。
```

`7782c8b` 修复了一个连接层 bug，但没有解决流式、第一轮 reasoning 和长等待问题。

下一轮交叉验证应从诊断日志与失败测试开始，不应继续替换可执行文件或做真实外网搜索。
