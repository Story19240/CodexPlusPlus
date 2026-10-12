# Web Search 功能验证报告

日期：2026-10-12  
分支：`codex/add-web-search`  
功能提交：`8b60569 feat(protocol): execute web search through Exa or Tavily`  
修复提交：`7782c8b fix(protocol): forward buffered web search streams`

## 1. 验证范围

本报告覆盖：

- Exa / Tavily 搜索 provider 的请求与解析契约
- Chat Completions 原生 `web_search` 工具转换
- 第一轮模型响应中的 `web_search` 调用识别
- 搜索结果追加为 `role=tool` 的二段模型请求
- 二段请求强制 `stream=true`
- Responses / Chat SSE 转换与本地 helper 下发
- 管理器 Web 搜索设置页
- 设置默认关闭、启用后的持久化

不覆盖真实 Exa / Tavily 账号链路验收。当前测试使用 wiremock 仿真服务，没有消耗真实搜索额度。

## 2. 实测发现的问题

### 2.1 首次验证时的两个非代码问题

1. 第一次只执行了 `cargo build --workspace --release`，没有先执行 `npm run vite:build`，导致 Tauri exe 内嵌的仍是旧的 `dist` 前端资源。管理器里因此看不到「Web 搜索」页签。
2. 第一次实测时 `webSearch.enabled` 为 `false`，请求自然没有进入服务端搜索链路。

处理：

- 已执行 `npm run vite:build`，确认新 bundle 中包含 `enhance-tab-webSearch` / `enhance-pane-webSearch`。
- 用户启用并保存后，设置文件中 `webSearch.enabled` 为 `true`。

### 2.2 发现的代码缺陷

启用 Web 搜索后，`protocol_proxy` 会先读取完整第一轮 Chat 流，判断模型是否真的发起了 `web_search` 调用。

问题出在“没有发起搜索调用”的分支：

- `protocol_proxy` 已经把第一轮响应转换成完整 Responses SSE，并放入 `UpstreamProxyResponse.buffered_body`
- 该对象没有 `response: Some(reqwest::Response)`
- `launcher` 的流式分支仍然调用 `upstream.take_response()`
- `take_response()` 返回“缺少上游响应”，本地连接被断开

这解释了实测中的现象：

- 上游第一轮已经返回 HTTP 200
- 本地 helper 没有把已缓冲的 SSE 发回 Codex
- Codex 表现为重连或无响应

## 3. 修复内容

修复提交 `7782c8b` 在 launcher 中新增缓冲流分支：

```text
if upstream.buffered_body.is_some()
    -> read_body()
    -> write_http_stream_headers()
    -> write_all(body)
    -> log helper.protocol_proxy_stream_ok
```

这样两种路径都有明确处理：

- 有 `web_search` 调用：执行 Exa / Tavily，再发起二段模型请求
- 无 `web_search` 调用：直接下发已转换的完整 Responses SSE

新增回归测试：

```text
buffered_web_search_response_is_sent_without_live_response
```

该测试构造了一个只有 `buffered_body`、没有 `response` 的 `UpstreamProxyResponse`，验证 launcher 能直接返回 HTTP 200 + `text/event-stream` + `response.completed`，不再调用 `take_response()`。

## 4. 验证结果

### 4.1 Rust

```powershell
cargo test -p codex-plus-core --lib web_search
```

结果：

```text
14 passed
```

覆盖：

- Exa 请求头、请求体、结果解析
- Tavily Bearer 认证、请求体、结果解析
- Chat JSON / SSE 工具调用解析
- 二段请求包含 assistant tool call 和 tool result
- 二段请求强制流式
- 搜索后的端到端编排
- 设置默认关闭、保存、update patch 合并
- 缓冲 SSE 无 live response 时的 launcher 下发

```powershell
cargo test -p codex-plus-core --lib buffered_web_search_response_is_sent_without_live_response
```

结果：

```text
1 passed
```

```powershell
cargo check -p codex-plus-core
```

结果：通过。仅有仓库既有 warnings。

```powershell
cargo test -p codex-plus-core --test launcher
```

结果：

```text
97 passed
1 failed
```

失败项：

```text
app_paths_resolves_portable_current_link_to_directory_version
```

失败原因是本机 Windows 权限：

```text
Os error 1314: 客户端没有所需的特权
```

该测试需要创建 symlink，与本分支改动无关，也不是 Web Search 逻辑失败。

### 4.2 前端

```powershell
npm run check
```

结果：通过。

```powershell
node --test src/web-search-settings.test.ts src/enhancement-settings.test.ts src/enhancement-navigation.test.ts
```

结果：

```text
14 passed
```

覆盖：

- Web Search 默认关闭
- provider 切换
- API Key / 环境变量校验
- 数字范围归一化
- 增强页签挂载与显示控制
- 密钥输入使用 password 类型

### 4.3 Release 构建

修复版已编译到独立目录，避免覆盖正在运行的程序：

```text
D:\Codex\codexplusplus\CodexPlusPlus\target\web-search-fix-test\release\codex-plus-plus.exe
```

构建命令：

```powershell
cargo build -p codex-plus-manager --release --target-dir target/web-search-fix-test
```

结果：构建成功。

## 5. 现场诊断要点

功能开启后，本地请求应满足：

1. `helper.request`：`POST /v1/responses`
2. `protocol_proxy.upstream_request`：上游 Chat Completions 请求
3. `protocol_proxy.upstream_response`：上游 HTTP 200
4. 若模型没有调用搜索：`helper.protocol_proxy_stream_ok`
5. 若模型调用搜索：应出现 Exa / Tavily 请求，随后出现二段模型请求，最终 `helper.protocol_proxy_stream_ok`

当前代码尚未给 Exa / Tavily 执行过程和二段请求添加专属诊断事件。下一轮如继续排查真实链路，应先补以下事件：

```text
web_search.call_detected
web_search.request
web_search.response
web_search.followup_request
web_search.followup_response
web_search.failed
```

## 6. 尚未完成的验证

- 未使用真实 Exa API Key 发起外网搜索
- 未使用真实 Tavily API Key 发起外网搜索
- 未在修复版 exe 上重新执行完整 Codex 桌面端真实对话
- 未验证超大上下文下的二段模型请求成功率

## 7. 结论

本次复查确认了一个可复现的本地断流缺陷，并已修复：

```text
启用 Web Search 后，第一轮没有工具调用时，buffered SSE 被 launcher 丢弃并断开连接。
```

修复已有回归测试保护。当前代码层面的聚焦测试全部通过；真实搜索服务和修复版桌面端链路仍需用户使用新编译产物做一次实测。
