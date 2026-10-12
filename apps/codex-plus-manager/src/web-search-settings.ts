export type WebSearchSettings = {
  enabled: boolean;
  provider: "exa" | "tavily";
  exaApiKey: string;
  exaApiKeyEnv: string;
  tavilyApiKey: string;
  tavilyApiKeyEnv: string;
  resultLimit: number;
  timeoutSeconds: number;
};

export type WebSearchSettingsIssue = "apiKey" | "apiKeyEnv";

export function defaultWebSearchSettings(): WebSearchSettings {
  return {
    enabled: false,
    provider: "exa",
    exaApiKey: "",
    exaApiKeyEnv: "EXA_API_KEY",
    tavilyApiKey: "",
    tavilyApiKeyEnv: "TAVILY_API_KEY",
    resultLimit: 5,
    timeoutSeconds: 20,
  };
}

export function normalizeWebSearchSettings(
  value: Partial<WebSearchSettings> | undefined,
): WebSearchSettings {
  const defaults = defaultWebSearchSettings();
  const text = (entry: unknown, fallback = "") =>
    typeof entry === "string" ? entry.trim() : fallback;
  const number = (entry: unknown, fallback: number, min: number, max: number) => {
    const parsed = typeof entry === "number" && Number.isFinite(entry) ? Math.round(entry) : fallback;
    return Math.min(max, Math.max(min, parsed));
  };
  return {
    enabled: value?.enabled === true,
    provider: value?.provider === "tavily" ? "tavily" : "exa",
    exaApiKey: text(value?.exaApiKey),
    exaApiKeyEnv: text(value?.exaApiKeyEnv, defaults.exaApiKeyEnv),
    tavilyApiKey: text(value?.tavilyApiKey),
    tavilyApiKeyEnv: text(value?.tavilyApiKeyEnv, defaults.tavilyApiKeyEnv),
    resultLimit: number(value?.resultLimit, defaults.resultLimit, 1, 20),
    timeoutSeconds: number(value?.timeoutSeconds, defaults.timeoutSeconds, 1, 60),
  };
}

export function webSearchSettingsIssue(settings: WebSearchSettings): WebSearchSettingsIssue | null {
  if (!settings.enabled) return null;
  const directKey = settings.provider === "exa" ? settings.exaApiKey : settings.tavilyApiKey;
  const envName = settings.provider === "exa" ? settings.exaApiKeyEnv : settings.tavilyApiKeyEnv;
  if (!directKey && !envName) return "apiKey";
  if (envName && !/^[A-Za-z_][A-Za-z0-9_]*$/.test(envName)) return "apiKeyEnv";
  return null;
}
