import assert from "node:assert/strict";
import { test } from "node:test";
import {
  defaultWebSearchSettings,
  normalizeWebSearchSettings,
  webSearchSettingsIssue,
} from "./web-search-settings.ts";

test("web search defaults are disabled and provider settings normalize safely", () => {
  const defaults = defaultWebSearchSettings();
  assert.equal(defaults.enabled, false);
  assert.equal(defaults.provider, "exa");
  assert.equal(defaults.resultLimit, 5);
  assert.equal(defaults.timeoutSeconds, 20);

  const normalized = normalizeWebSearchSettings({
    enabled: true,
    provider: "tavily",
    exaApiKey: " direct ",
    tavilyApiKeyEnv: "TAVILY_KEY",
    resultLimit: 99,
    timeoutSeconds: 0,
  });
  assert.equal(normalized.exaApiKey, "direct");
  assert.equal(normalized.resultLimit, 20);
  assert.equal(normalized.timeoutSeconds, 1);
});

test("enabled web search requires a valid key source", () => {
  const settings = defaultWebSearchSettings();
  assert.equal(webSearchSettingsIssue(settings), null);

  settings.enabled = true;
  settings.exaApiKeyEnv = "";
  assert.equal(webSearchSettingsIssue(settings), "apiKey");

  settings.exaApiKey = "exa-key";
  assert.equal(webSearchSettingsIssue(settings), null);

  settings.exaApiKey = "";
  settings.exaApiKeyEnv = "1BAD_ENV";
  assert.equal(webSearchSettingsIssue(settings), "apiKeyEnv");
});
