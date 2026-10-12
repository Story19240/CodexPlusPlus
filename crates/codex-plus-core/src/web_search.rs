use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::settings::{WebSearchProvider, WebSearchSettings};

const EXA_SEARCH_ENDPOINT: &str = "https://api.exa.ai/search";
const TAVILY_SEARCH_ENDPOINT: &str = "https://api.tavily.com/search";
pub const MAX_WEB_SEARCH_RESPONSE_BODY_BYTES: usize = 1024 * 1024;
const MAX_RESULT_TEXT_CHARS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchToolCall {
    pub call_id: String,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchToolOutput {
    pub call_id: String,
    pub query: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedChatResponse {
    pub id: String,
    pub model: String,
    pub message: Value,
    pub finish_reason: Option<String>,
    pub usage: Option<Value>,
}

impl ParsedChatResponse {
    pub fn web_search_calls(&self) -> anyhow::Result<Vec<WebSearchToolCall>> {
        let Some(tool_calls) = self.message.get("tool_calls").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        let mut calls = Vec::new();
        for tool_call in tool_calls {
            let function = tool_call.get("function").unwrap_or(&Value::Null);
            let name = function.get("name").and_then(Value::as_str).unwrap_or("");
            if name != "web_search" {
                bail!("服务端 Web 搜索只支持单独的 web_search 工具调用");
            }
            let call_id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("web_search 工具调用缺少 id"))?;
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let query = query_from_arguments(arguments)
                .with_context(|| format!("web_search 调用 {call_id} 缺少有效查询词"))?;
            calls.push(WebSearchToolCall {
                call_id: call_id.to_string(),
                query,
            });
        }
        Ok(calls)
    }
}

pub async fn execute_web_search(
    settings: &WebSearchSettings,
    query: &str,
) -> anyhow::Result<Vec<WebSearchResult>> {
    let endpoint = match settings.provider {
        WebSearchProvider::Exa => EXA_SEARCH_ENDPOINT,
        WebSearchProvider::Tavily => TAVILY_SEARCH_ENDPOINT,
    };
    execute_web_search_at(settings, query, endpoint).await
}

async fn execute_web_search_at(
    settings: &WebSearchSettings,
    query: &str,
    endpoint: &str,
) -> anyhow::Result<Vec<WebSearchResult>> {
    let query = query.trim();
    if query.is_empty() {
        bail!("Web 搜索查询词不能为空");
    }
    let result_limit = settings.result_limit.clamp(1, 20) as usize;
    let timeout = Duration::from_secs(settings.timeout_seconds.clamp(1, 60));
    let connect_timeout = Duration::from_secs(5).min(timeout);
    let client = reqwest::Client::builder()
        .user_agent(format!(
            "CodexPlusPlus-WebSearch/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .connect_timeout(connect_timeout)
        .timeout(timeout)
        .build()
        .context("创建 Web 搜索 HTTP client 失败")?;

    let request = match settings.provider {
        WebSearchProvider::Exa => {
            let api_key =
                configured_api_key(&settings.exa_api_key, &settings.exa_api_key_env, "Exa")?;
            client
                .post(endpoint)
                .header("x-api-key", api_key)
                .json(&json!({
                    "query": query,
                    "numResults": result_limit,
                    "contents": { "text": true }
                }))
        }
        WebSearchProvider::Tavily => {
            let api_key = configured_api_key(
                &settings.tavily_api_key,
                &settings.tavily_api_key_env,
                "Tavily",
            )?;
            client.post(endpoint).bearer_auth(api_key).json(&json!({
                "query": query,
                "max_results": result_limit,
                "search_depth": "basic"
            }))
        }
    };

    let mut response = request.send().await.context("Web 搜索请求失败")?;
    let status = response.status();
    if !status.is_success() {
        let body = read_limited_body(&mut response).await.unwrap_or_default();
        bail!(
            "{} 搜索返回 HTTP {}: {}",
            provider_label(settings.provider),
            status.as_u16(),
            error_preview(&body)
        );
    }
    let body = read_limited_body(&mut response).await?;
    let results = match settings.provider {
        WebSearchProvider::Exa => parse_exa_results(&body)?,
        WebSearchProvider::Tavily => parse_tavily_results(&body)?,
    };
    Ok(results
        .into_iter()
        .take(result_limit)
        .map(|result| WebSearchResult {
            content: truncate_chars(&result.content, MAX_RESULT_TEXT_CHARS),
            ..result
        })
        .collect())
}

pub async fn execute_web_search_calls(
    settings: &WebSearchSettings,
    calls: &[WebSearchToolCall],
) -> anyhow::Result<Vec<WebSearchToolOutput>> {
    execute_web_search_calls_at(settings, calls, None).await
}

pub(crate) async fn execute_web_search_calls_at(
    settings: &WebSearchSettings,
    calls: &[WebSearchToolCall],
    search_endpoint: Option<&str>,
) -> anyhow::Result<Vec<WebSearchToolOutput>> {
    let mut outputs = Vec::new();
    for call in calls {
        let results = match search_endpoint {
            Some(endpoint) => execute_web_search_at(settings, &call.query, endpoint).await,
            None => execute_web_search(settings, &call.query).await,
        }
        .with_context(|| format!("web_search 调用 {} 执行失败", call.call_id))?;
        outputs.push(WebSearchToolOutput {
            call_id: call.call_id.clone(),
            query: call.query.clone(),
            content: format_search_results(&call.query, &results),
        });
    }
    Ok(outputs)
}

pub fn parse_chat_response(body: &[u8], content_type: &str) -> anyhow::Result<ParsedChatResponse> {
    let text = std::str::from_utf8(body).context("Chat 上游响应不是有效 UTF-8")?;
    if content_type.contains("text/event-stream") || text.trim_start().starts_with("data:") {
        parse_chat_sse_response(text)
    } else {
        parse_chat_json_response(text)
    }
}

fn parse_chat_json_response(text: &str) -> anyhow::Result<ParsedChatResponse> {
    let body: Value = serde_json::from_str(text).context("Chat 上游 JSON 解析失败")?;
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| anyhow::anyhow!("Chat 上游响应缺少 choices"))?;
    let message = choice
        .get("message")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Chat 上游响应缺少 message"))?;
    Ok(ParsedChatResponse {
        id: body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("chatcmpl_web_search")
            .to_string(),
        model: body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        message,
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        usage: body.get("usage").cloned(),
    })
}

fn parse_chat_sse_response(text: &str) -> anyhow::Result<ParsedChatResponse> {
    let normalized = text.replace("\r\n", "\n");
    let mut id = String::new();
    let mut model = String::new();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: BTreeMap<u64, Value> = BTreeMap::new();
    let mut finish_reason = None;
    let mut usage = None;
    let mut saw_done = false;

    for block in normalized.split("\n\n") {
        let mut event_name = None;
        for line in block.lines() {
            if let Some(event) = line.strip_prefix("event:").map(str::trim) {
                event_name = Some(event.to_string());
            }
        }
        let data = block
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if data.trim().is_empty() {
            continue;
        }
        if data.trim() == "[DONE]" {
            saw_done = true;
            break;
        }
        let chunk: Value = serde_json::from_str(&data).context("Chat SSE 数据解析失败")?;
        if event_name.as_deref() == Some("error") {
            let message = chunk
                .pointer("/error/message")
                .or_else(|| chunk.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Chat 上游返回流式错误");
            bail!("{message}");
        }
        if let Some(error) = chunk.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Chat 上游返回流式错误");
            bail!("{message}");
        }
        if id.is_empty() {
            if let Some(value) = chunk.get("id").and_then(Value::as_str) {
                id = value.to_string();
            }
        }
        if model.is_empty() {
            if let Some(value) = chunk.get("model").and_then(Value::as_str) {
                model = value.to_string();
            }
        }
        if let Some(value) = chunk.get("usage").filter(|value| !value.is_null()) {
            usage = Some(value.clone());
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            continue;
        };
        if let Some(value) = choice.get("finish_reason").and_then(Value::as_str) {
            finish_reason = Some(value.to_string());
        }
        let Some(delta) = choice.get("delta") else {
            continue;
        };
        if let Some(value) = delta.get("content").and_then(Value::as_str) {
            content.push_str(value);
        }
        if let Some(value) = delta
            .get("reasoning_content")
            .or_else(|| {
                delta
                    .get("reasoning")
                    .and_then(|value| value.get("content"))
            })
            .and_then(Value::as_str)
        {
            reasoning.push_str(value);
        }
        if let Some(deltas) = delta.get("tool_calls").and_then(Value::as_array) {
            for tool_call in deltas {
                let index = tool_call
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(tool_calls.len() as u64);
                let state = tool_calls.entry(index).or_insert_with(|| {
                    json!({ "id": "", "type": "function", "function": { "name": "", "arguments": "" } })
                });
                if let Some(value) = tool_call.get("id").and_then(Value::as_str) {
                    state["id"] = json!(value);
                }
                if let Some(function) = tool_call.get("function") {
                    if let Some(value) = function.get("name").and_then(Value::as_str) {
                        state["function"]["name"] = json!(value);
                    }
                    if let Some(value) = function.get("arguments").and_then(Value::as_str) {
                        let arguments = state
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        state["function"]["arguments"] = json!(format!("{arguments}{value}"));
                    }
                }
            }
        }
    }
    if !saw_done && finish_reason.is_none() {
        bail!("Chat 上游在完成事件前结束了响应流");
    }

    let mut message = json!({ "role": "assistant" });
    if !content.is_empty() {
        message["content"] = json!(content);
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls.into_values().collect::<Vec<_>>());
    }
    Ok(ParsedChatResponse {
        id: if id.is_empty() {
            "chatcmpl_web_search".to_string()
        } else {
            id
        },
        model,
        message,
        finish_reason,
        usage,
    })
}

pub fn build_web_search_followup_request(
    chat_request: &Value,
    first_response: &ParsedChatResponse,
    outputs: &[WebSearchToolOutput],
) -> anyhow::Result<Value> {
    let mut request = chat_request.clone();
    let messages = request
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .context("Chat 请求缺少 messages，无法追加搜索结果")?;
    messages.push(first_response.message.clone());
    for output in outputs {
        if output.call_id.trim().is_empty() {
            bail!("Web 搜索结果缺少 tool_call_id");
        }
        messages.push(json!({
            "role": "tool",
            "tool_call_id": output.call_id,
            "content": output.content
        }));
    }

    // 二段请求必须流式拿响应头，避免大上下文下非流式上游超过 30 秒头部超时。
    request["stream"] = json!(true);
    let mut stream_options = request
        .get("stream_options")
        .cloned()
        .unwrap_or_else(|| json!({}));
    stream_options["include_usage"] = json!(true);
    request["stream_options"] = stream_options;
    if request.get("tool_choice").is_some() {
        request["tool_choice"] = json!("auto");
    }
    Ok(request)
}

pub fn format_search_results(query: &str, results: &[WebSearchResult]) -> String {
    if results.is_empty() {
        return format!("No web search results for query: {query}");
    }
    let mut output = format!("Web search results for query: {query}\n");
    for (index, result) in results.iter().enumerate() {
        output.push_str(&format!(
            "\n{}. {}\nURL: {}\nContent: {}\n",
            index + 1,
            result.title,
            result.url,
            result.content
        ));
    }
    output
}

fn query_from_arguments(arguments: &str) -> anyhow::Result<String> {
    let parsed: Value = serde_json::from_str(arguments).context("web_search 参数不是有效 JSON")?;
    for key in ["query", "input", "q"] {
        if let Some(query) = parsed.get(key).and_then(Value::as_str) {
            let query = query.trim();
            if !query.is_empty() {
                return Ok(query.to_string());
            }
        }
    }
    if let Some(queries) = parsed.get("queries").and_then(Value::as_array) {
        let query = queries
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|query| !query.is_empty());
        if let Some(query) = query {
            return Ok(query.to_string());
        }
    }
    bail!("web_search 参数缺少 query/input/queries")
}

fn configured_api_key(direct: &str, env_name: &str, provider: &str) -> anyhow::Result<String> {
    let direct = direct.trim();
    if !direct.is_empty() {
        return Ok(direct.to_string());
    }
    let env_name = env_name.trim();
    if !env_name.is_empty() {
        let value = std::env::var(env_name)
            .with_context(|| format!("读取 {provider} API Key 环境变量 {env_name} 失败"))?;
        let value = value.trim();
        if !value.is_empty() {
            return Ok(value.to_string());
        }
    }
    bail!("{provider} API Key 未配置")
}

fn provider_label(provider: WebSearchProvider) -> &'static str {
    match provider {
        WebSearchProvider::Exa => "Exa",
        WebSearchProvider::Tavily => "Tavily",
    }
}

#[derive(Debug, Deserialize)]
struct ExaSearchResponse {
    #[serde(default)]
    results: Vec<ExaSearchResult>,
}

#[derive(Debug, Deserialize)]
struct ExaSearchResult {
    title: Option<String>,
    url: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TavilySearchResponse {
    #[serde(default)]
    results: Vec<TavilySearchResult>,
}

#[derive(Debug, Deserialize)]
struct TavilySearchResult {
    title: Option<String>,
    url: Option<String>,
    content: Option<String>,
}

fn parse_exa_results(body: &[u8]) -> anyhow::Result<Vec<WebSearchResult>> {
    let parsed: ExaSearchResponse = serde_json::from_slice(body).context("Exa 响应解析失败")?;
    Ok(parsed
        .results
        .into_iter()
        .filter_map(|result| normalized_result(result.title, result.url, result.text))
        .collect())
}

fn parse_tavily_results(body: &[u8]) -> anyhow::Result<Vec<WebSearchResult>> {
    let parsed: TavilySearchResponse =
        serde_json::from_slice(body).context("Tavily 响应解析失败")?;
    Ok(parsed
        .results
        .into_iter()
        .filter_map(|result| normalized_result(result.title, result.url, result.content))
        .collect())
}

fn normalized_result(
    title: Option<String>,
    url: Option<String>,
    content: Option<String>,
) -> Option<WebSearchResult> {
    let url = url?.trim().to_string();
    if url.is_empty() {
        return None;
    }
    Some(WebSearchResult {
        title: title.unwrap_or_default().trim().to_string(),
        url,
        content: content.unwrap_or_default().trim().to_string(),
    })
}

async fn read_limited_body(response: &mut reqwest::Response) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.context("读取 Web 搜索响应失败")? {
        if body.len().saturating_add(chunk.len()) > MAX_WEB_SEARCH_RESPONSE_BODY_BYTES {
            bail!("Web 搜索响应超过 1 MiB 上限");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn error_preview(body: &[u8]) -> String {
    truncate_chars(&String::from_utf8_lossy(body), 512)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn settings(provider: WebSearchProvider, api_key: &str) -> WebSearchSettings {
        WebSearchSettings {
            enabled: true,
            provider,
            exa_api_key: if provider == WebSearchProvider::Exa {
                api_key.to_string()
            } else {
                String::new()
            },
            exa_api_key_env: String::new(),
            tavily_api_key: if provider == WebSearchProvider::Tavily {
                api_key.to_string()
            } else {
                String::new()
            },
            tavily_api_key_env: String::new(),
            result_limit: 2,
            timeout_seconds: 5,
        }
    }

    #[tokio::test]
    async fn exa_search_sends_key_query_and_parses_text_results() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(header("x-api-key", "exa-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "title": "Exa result", "url": "https://example.com/exa", "text": "Exa content" },
                    { "title": "Second", "url": "https://example.com/two", "text": "Two" },
                    { "title": "Dropped", "text": "No URL" }
                ]
            })))
            .mount(&server)
            .await;

        let results = execute_web_search_at(
            &settings(WebSearchProvider::Exa, "exa-key"),
            "latest rust news",
            &format!("{}/search", server.uri()),
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Exa result");
        assert_eq!(results[0].url, "https://example.com/exa");
        assert_eq!(results[0].content, "Exa content");
        let request = server.received_requests().await.unwrap().pop().unwrap();
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["query"], "latest rust news");
        assert_eq!(body["numResults"], 2);
    }

    #[tokio::test]
    async fn tavily_search_uses_bearer_auth_and_parses_content_results() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .and(header("authorization", "Bearer tavily-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "title": "Tavily result", "url": "https://example.com/tavily", "content": "Tavily content" }
                ]
            })))
            .mount(&server)
            .await;

        let results = execute_web_search_at(
            &settings(WebSearchProvider::Tavily, "tavily-key"),
            "codex release",
            &format!("{}/search", server.uri()),
        )
        .await
        .unwrap();

        assert_eq!(
            results,
            vec![WebSearchResult {
                title: "Tavily result".to_string(),
                url: "https://example.com/tavily".to_string(),
                content: "Tavily content".to_string(),
            }]
        );
        let request = server.received_requests().await.unwrap().pop().unwrap();
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["query"], "codex release");
        assert_eq!(body["max_results"], 2);
    }

    #[test]
    fn chat_stream_parser_and_followup_request_preserve_tool_contract() {
        let stream = concat!(
            "data: {\"id\":\"chatcmpl-search\",\"model\":\"fixture-model\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_search\",\"function\":{\"name\":\"web_search\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"query\\\":\\\"Codex\\\"}\"}}]}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let parsed = parse_chat_response(stream.as_bytes(), "text/event-stream").unwrap();
        let calls = parsed.web_search_calls().unwrap();
        assert_eq!(
            calls,
            vec![WebSearchToolCall {
                call_id: "call_search".to_string(),
                query: "Codex".to_string(),
            }]
        );

        let outputs = vec![WebSearchToolOutput {
            call_id: "call_search".to_string(),
            query: "Codex".to_string(),
            content: "Web search result".to_string(),
        }];
        let followup = build_web_search_followup_request(
            &json!({
                "model": "fixture-model",
                "messages": [{ "role": "user", "content": "hi" }],
                "stream": false,
                "tool_choice": { "type": "web_search" },
            }),
            &parsed,
            &outputs,
        )
        .unwrap();

        assert_eq!(followup["stream"], true);
        assert_eq!(followup["stream_options"]["include_usage"], true);
        assert_eq!(followup["tool_choice"], "auto");
        let messages = followup["messages"].as_array().unwrap();
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["name"],
            "web_search"
        );
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_search");
        assert_eq!(messages[2]["content"], "Web search result");
    }

    #[test]
    fn chat_stream_parser_rejects_error_and_truncated_streams() {
        let error = parse_chat_response(
            b"event: error\ndata: {\"message\":\"upstream failed\"}\n\n",
            "text/event-stream",
        )
        .unwrap_err();
        assert!(error.to_string().contains("upstream failed"));

        let truncated = parse_chat_response(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "text/event-stream",
        )
        .unwrap_err();
        assert!(truncated.to_string().contains("完成事件"));
    }
}
