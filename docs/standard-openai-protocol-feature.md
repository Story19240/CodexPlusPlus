# 新增「纯标准协议」供应商开关

## 背景与问题

Codex++ 的协议转换层会根据**模型名**为上游请求注入厂商私有 reasoning 参数。在 `protocol_proxy.rs::infer_chat_reasoning_style` 中，模型名含 `minimax` ⟹ 注入 `reasoning_split`；其他还有 `thinking`（glm/kimi）、`enable_thinking`（qwen）、`reasoning{effort}`（openrouter）等"方言"。

这套设计隐含假设：**模型名命中 ⟹ 上游端点就是该厂商官方**。当用户把 MiniMax 模型挂在 **NVIDIA 的 OpenAI 兼容端点**上时，假设链断裂：

```
应用报错：Unsupported parameter(s): reasoning_split
type: bad_response_status_code
```

NVIDIA 只认标准 OpenAI Chat Completions 协议，拒绝 `reasoning_split` 这类厂商方言。`reasoning_split` 只是第一个翻车的；若 NVIDIA 对其他方言也走严格校验，下一个报错的就是 `thinking`（glm）或 `enable_thinking`（qwen）。

## 方案选型讨论

会话中评估了三个方向：

1. **newapi 网关层捕获并剥离该参数**——治标，需对每个新方言字段各配过滤规则，跟随 codex++ 升级维护负担大。
2. **按端点（base_url 白名单）判断，而非纯模型名**——治本，但要维护官方域名表，覆盖所有方言模型。
3. **给 RelayProfile 加供应商级开关，打开后整段跳过方言注入**——最省心，对第三方聚合端点一刀切；新增方言字段也自动安全。

用户与作者经多轮讨论后选定 **方案 B（供应商级开关）**：面向第三方聚合端点天然失败安全（fail-closed），默认对老配置零影响（opt-in，默认 false），且一次性挡掉所有方言而非只补 minimax。

## 实现设计

### 核心语义

新增字段 `standardOpenaiProtocol: bool`（RelayProfile 级，opt-in，默认 false）。打开后：

- **跳过所有厂商方言字段注入**：`reasoning_split`(minimax)、`thinking`(glm/kimi)、`enable_thinking`(qwen/siliconflow)、`reasoning{effort}`(openrouter)
- **保留标准 `reasoning_effort` 注入路径**：只给 deepseek/gpt5+/o 系列等支持该字段的模型注；该字段是 OpenAI 标准，NVIDIA 也认，丢掉反而损失推理强度控制
- 其他模型（minimax/glm/qwen 在 NVIDIA 上）则完全不发任何 reasoning 参数，交给上游默认行为

实现方式：`apply_chat_reasoning_options` 新增 `standard: bool` 参数。`standard=true` 时把推理 `style` 强制为 `ChatReasoningStyle::Default`——方言分支自然空过，而 `Default` 风格下的标准 `reasoning_effort` 路径保留。

### 零破坏设计

- 保留旧函数签名 `responses_to_chat_completions(body: Value)`，内部委托新函数并传 `false`。**现有 25 处测试调用无需改动**，保证现有行为不变（回归守护）。
- 新函数 `responses_to_chat_completions_with_options(body, standard)` 只在协议转换的真实入口 `upstream_request_parts` 处取 `relay.standard_openai_protocol` 传入。

## 改动清单

11 个文件，约 156 行（含 1 个 npm install 带来的 package-lock.json，PR 前可视情况剔除）。核心 10 个：

| 文件 | 改动 |
| --- | --- |
| `crates/codex-plus-core/src/settings.rs` | `RelayProfile` 加字段 `standard_openai_protocol: bool`（`#[serde(rename="standardOpenaiProtocol", default)]`），Default 实现及 2 个工厂补 `false` |
| `crates/codex-plus-core/src/protocol_proxy.rs` | 新增 `responses_to_chat_completions_with_options`；旧函数委托传 `false`；`apply_chat_reasoning_options` 加 `standard` 参数；`standard=true` 时 style 强制 `Default`；上游调用点传 `relay.standard_openai_protocol` |
| `crates/codex-plus-core/src/ccs_import.rs` / `provider_import.rs` | 各 1 处 RelayProfile 构造补 `standard_openai_protocol: false` |
| `crates/codex-plus-core/tests/protocol_proxy.rs` | 新增 `responses_request_standard_protocol_strips_vendor_reasoning_dialects`：验证 minimax/glm/qwen/openrouter 方言被剥离、deepseek/gpt5 标准 `reasoning_effort` 保留、默认路径仍注入 `reasoning_split`（回归守护） |
| `crates/codex-plus-core/tests/launcher.rs` | 1 处 RelayProfile 构造补字段 |
| `apps/codex-plus-manager/src/App.tsx` | RelayProfile 类型加字段；6 处默认值工厂补 `false`；`deriveRelayProfileFromFiles` 透传；供应商配置页「上游协议」下方新增「纯标准协议」勾选框（占满整行） |
| `apps/codex-plus-manager/src/styles.css` | 新增 `.relay-field-standard .inline-check { width: 100%; }`，让带边框的勾选盒子撑满整行 |
| `apps/codex-plus-manager/src/i18n-en.ts` | 3 条中英文翻译（开关标题、勾选说明、提示行） |
| `apps/codex-plus-manager/src/model-windows.test.ts` | 1 处 RelayProfile 字面量补新字段（保持 strict TS 通过） |

## 关键代码位置

- 协议转换核心（开关生效点）：`crates/codex-plus-core/src/protocol_proxy.rs` 的 `apply_chat_reasoning_options`（`standard` 分支）
- 上游调用点（开关读取点）：`crates/codex-plus-core/src/protocol_proxy.rs` 的 `upstream_request_parts`（`relay.standard_openai_protocol`）
- 模型名 ⟹ 方言启发式：`crates/codex-plus-core/src/protocol_proxy.rs` 的 `infer_chat_reasoning_style`
- 前端开关：`apps/codex-plus-manager/src/App.tsx` 供应商配置面板「上游协议」正下方
- 字段定义/序列化：`crates/codex-plus-core/src/settings.rs` 的 `RelayProfile`

## 验证结果

本机安装 Rust 工具链（stable-msvc）后逐项验证：

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 后端单测 | `cargo test -p codex-plus-core --test protocol_proxy` | **50 passed; 0 failed**，含新增 `responses_request_standard_protocol_strips_vendor_reasoning_dialects ... ok` |
| 后端全量 | `cargo test -p codex-plus-core` | 93 passed; 1 failed（`list_targets_can_query_ipv6_loopback_cdp_endpoint`，CDP 网络 `os error 10061/10013`，与本次改动无关） |
| 前端类型 | `npm run check`（tsc --noEmit） | 零错误 |
| 前端单测 | `npm test`（node --test） | 36 tests, 36 pass, 0 fail |

本地手动启动 `codex-plus-plus-manager.exe`（debug 构建产物），在供应商配置勾选「纯标准协议」→ 保存后功能正常，UI 布局占满整行、提示行文案已更新。

## 验证中发现并修复的两处遗漏

1. **Rust 端 RelayProfile 构造未覆盖**：最初静态覆盖只扫了前端，Rust 端还有 6 处用 `sub2api_multiplier: String::new()` 的构造（`ccs_import.rs`/`provider_import.rs` 各 1，`settings.rs` 3，`launcher.rs` 1）编译报 `missing field`，逐一补 `standard_openai_protocol: false`。
2. **测试断言写错**：`standard=true` 时 deepseek 也被强制成 Default 走**通用映射**（`xhigh→xhigh`），而非 DeepSeek 私有映射（`→max`）。断言由 `"max"` 改为 `"xhigh"` 后通过。这是预期行为：标准协议下所有模型走统一标准映射。

## 本地构建/启动步骤

| 步骤 | 命令 | 说明 |
| --- | --- | --- |
| 装前端依赖（一次性） | 在 `apps/codex-plus-manager` 跑 `npm install` | 约 56s，101 包 |
| 重建前端产物 | `npm run vite:build` | 写 `apps/codex-plus-manager/dist` |
| 重建 manager exe | 在仓库根跑 `cargo build -p codex-plus-manager` | debug 产物 |
| 重建 launcher exe（可选，「重启 Codex++」按钮需要） | `cargo build -p codex-plus-launcher` | debug 产物 |

**入口（双击即可）**

- 管理工具：`D:\Codex\codexbridge\CodexPlusPlus\target\debug\codex-plus-plus-manager.exe`（右键以管理员身份运行）
- 静默启动 Codex 桌面应用：`D:\Codex\codexbridge\CodexPlusPlus\target\debug\codex-plus-plus.exe`

> manager 因涉及操作 Codex 官方应用/改 config.toml/CDP 注入，启动需管理员权限（否则 `os error 740`）。

## PR 说明

- **分支名建议**：`codex/standard-openai-protocol`
- **PR 目标**：`dev`（按照上游 CONTRIBUTING / MAINTAINERS 规则，feature PR 不应直接对 `main`）
- **package-lock.json**：`npm install` 产生的 30 行删改属于环境对齐，非本功能代码改动，PR 前可 `git checkout -- apps/codex-plus-manager/package-lock.json` 还原，保持 PR 只含功能改动
- **关联 issue**：若上游有「第三方端点方言参数导致 400」类 issue，可在 PR 描述中关联
- **未做**：未碰 CDP/launcher/会话管理等无关子系统；未改现有供应商的默认行为（opt-in）；旧函数签名保留（无 RPC/调用链破坏）