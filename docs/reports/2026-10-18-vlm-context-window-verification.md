# VLM 上下文窗口匹配与估算验证报告

日期：2026-10-18
范围：`crates/codex-plus-core`

## 结论

本次修复包含两组问题：

1. `modelWindows` 查表支持完整模型名、短名和双边去 provider 前缀归一化；多个 provider 剥头后撞名时不随机选择。
2. VLM 图片预处理不再用整包 `JSON bytes / 2` 估算上下文。优先复用同线程同模型的真实 `usage.total_tokens`，只估算锚点之后的新增消息；没有可用锚点时才退回语义文本估算。

主对话上下文窗口不受这次 VLM 修复影响。主窗口仍由 `model_catalog_json` 生成链路决定，`z-ai/glm-5.3` 等配置为 `1M` 的模型会继续在 catalog 中得到 `context_window=1000000`。

## 执行顺序

新的 VLM 请求时序如下：

1. 请求进入协议代理；
2. 清理泄漏到文本字段的 base64 图片数据；
3. 根据 `modelVlm` 判断图片处理模式；
4. `vlm` 模式先检查消息中是否存在结构化图片；
5. 没有图片时直接返回，不做 token 估算，也不写 `vlm_context_overflow`；
6. 有图片时解析窗口：
   - 完整模型名优先；
   - 请求短名回退；
   - 配置 key 剥 provider 后唯一命中时回退；
   - 多个 provider 撞名则视为配置冲突，不随机选择；
7. 优先读取同线程同模型的真实 usage 锚点；
8. 命中锚点时只估算锚点之后的新增消息；
9. 未命中锚点时使用语义文本估算；
10. 与 `context_window * 90%` 比较；
11. 超过阈值才跳过 VLM 图片分析并记录 overflow。

## 窗口匹配规则

`resolve_context_window()` 的匹配优先级：

1. 请求 `z-ai/glm-5.3`，配置 `z-ai/glm-5.3`：完整名命中；
2. 请求 `z-ai/glm-5.3`，配置 `glm-5.3`：请求短名命中；
3. 请求 `glm-5.3`，配置 `z-ai/glm-5.3`：双边归一化命中；
4. 配置同时有 `z-ai/glm-5.3` 与 `other/glm-5.3`，请求为 `glm-5.3`：不随机选择，回退到全局窗口。

最后兜底顺序不变：

```text
modelWindows
  → profile contextWindow
  → 272_000
```

`272_000` 只在模型窗口和全局窗口都缺失或无效时出现。

## 真实 usage 锚点

新增 `context_usage` 模块，在进程内维护最多 128 个 `thread_id + model` 锚点。

流程：

1. 请求进入 VLM 分支前，记录消息数量和边界消息 hash；
2. Chat 上游流结束时，从响应 usage 中取 `total_tokens`；
3. 将该 usage 绑定到最近请求边界；
4. 下一轮请求先校验边界消息 hash；
5. 校验通过后：

```text
estimated_tokens = usage.total_tokens
                 + semantic_estimate(messages[covered_message_count..])
```

这样避免重复计算 assistant 输出，也避免对整个历史重新估算。压缩、模型切换、线程切换或边界不匹配时不会使用旧锚点，而是回退到语义文本估算。

Chat web search 的 follow-up 请求也会重新登记边界，最终 usage 会绑定到 follow-up 后的请求形态，供下一轮复用。

## 语义文本估算

无锚点时的估算只统计模型可见文本字段：

```text
content
text
output
arguments
name
```

忽略：

```text
role
type
id
call_id
tool_call_id
image_url
input_image
JSON key 与转义开销
```

估算规则：

```text
ASCII: ceil(chars / 3)
非 ASCII: 1 token / 字符
每条消息: 8 token 固定开销
```

相比旧的 `serde_json::to_string(messages).len() / 2`，新估算不再把 JSON 结构、长 ID、图片 URL、转义字符和 base64 泄漏当作文本 token。该估算也只作为锚点缺失时的 fallback。

## 诊断日志

`vlm_context_overflow` 现在包含：

```json
{
  "context_window": 1000000,
  "effective_threshold": 900000,
  "estimated_tokens": 530000,
  "estimate_method": "actual_usage_plus_delta",
  "actual_context_used": 512000,
  "estimated_new_tokens": 18000,
  "skipped_images": 1
}
```

其中 `estimate_method` 可能是：

- `actual_usage_plus_delta`
- `semantic_text_v1`

纯文本请求即使触发 `modelVlm=vlm` 分支，也会因为没有结构化图片直接返回，不再产生误导性的 overflow 日志。

## 验证结果

### 定向测试

```text
cargo test -p codex-plus-core --lib -j1 resolve_context_window -- --test-threads=1
```

结果：

```text
6 passed / 0 failed
```

覆盖：

- 完整名 + 完整配置名；
- 完整名 + 短配置名；
- 短请求名 + 完整配置名；
- 多 provider 剥头后撞名回退。

```text
cargo test -p codex-plus-core --lib -j1 context_usage -- --test-threads=1
```

结果：

```text
3 passed / 0 failed
```

覆盖：

- usage 绑定最近请求边界；
- 模型不匹配不生成锚点；
- usage 缺失或为 0 不生成锚点。

```text
cargo test -p codex-plus-core --lib -j1 estimate_tokens -- --test-threads=1
```

结果：

```text
3 passed / 0 failed
```

覆盖：

- 估算随文本增长；
- transport-only 字段与图片块不计入；
- ASCII 与中文字符使用不同保守估算。

```text
cargo test -p codex-plus-core --lib -j1 strip_image_blocks -- --test-threads=1
```

结果：

```text
13 passed / 0 failed
```

新增关键用例：

```text
strip_image_blocks_uses_actual_usage_plus_delta
```

该用例构造了一个没有锚点时会误判 overflow 的长历史。使用真实 usage 锚点后，同一请求可以正常注入缓存图片描述，证明“真实 usage + 新增估算”生效。

### 全量测试

```text
cargo test -p codex-plus-core --lib -j1 -- --test-threads=1
```

结果：

```text
978 passed / 0 failed / 4 ignored
```

### 协议代理 integration 测试

```text
cargo test -p codex-plus-core --test protocol_proxy -j1 -- --test-threads=1
```

结果：

```text
179 passed / 0 failed
```

## 真实运行验证步骤

### 构建与启动结果

命令：

```text
cargo build --release -p codex-plus-manager
```

结果：

```text
Finished release profile [optimized] target(s)
```

首次构建时，运行中的旧实例锁住了 `target\release\codex-plus-plus.exe`，导致最后替换失败。停止旧实例后重新构建成功。

当前本地实例：

```text
PID: 20760
Path: D:\Codex\codexplusplus\CodexPlusPlus\target\release\codex-plus-plus.exe
Version: 1.10.0
Protocol proxy port: 127.0.0.1:57321
```

之前误占端口的旧安装版为：

```text
Path: D:\Software\CodexPlusPlus\codex-plus-plus.exe
Version: 1.8.0
```

该旧实例已停止，端口 `57321` 已由新版 `PID 20760` 监听。

### 后续实测步骤

1. 在 `z-ai/glm-5.3` 会话中先完成一次普通请求，让 Chat 上游返回 usage 并建立锚点；
2. 发起带图请求，观察日志：

```text
estimate_method = actual_usage_plus_delta
context_window = 1000000
```

3. 发起 web search follow-up，确认纯文本请求不再出现 `vlm_context_overflow`；
4. 如果出现 overflow，检查：

```text
actual_context_used
estimated_new_tokens
estimated_tokens
effective_threshold
```

此时可以区分是真实上下文接近 900K，还是新增内容确实过大。

## 边界与后续

- 主模型 catalog 窗口不受本修复影响；
- Chat 上游流可以建立真实 usage 锚点；
- 原生 Responses 与 Anthropic 流暂无 usage 观测接入，VLM 分支会退回语义文本估算；
- 若上游不返回 usage，也会退回语义文本估算；
- `272_000` 仍是最后兜底，不再是带前缀模型的默认窗口。
