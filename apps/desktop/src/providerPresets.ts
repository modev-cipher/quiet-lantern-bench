// OpenAI-compatible connection presets (owner feedback F2, 2026-08-29):
// normal users cannot know base URLs, so the add-connection form offers
// named presets that prefill the verified base URL plus the provider's
// data-handling link. "Custom" preserves the raw field exactly. Every URL
// below was verified against the provider's live documentation on
// 2026-08-29 (provider-provenance comment exception, AGENTS.md §5a); the
// repo's listing fetch appends `/models` to the base, so each base is the
// documented OpenAI-compatible root.
//
// Presets never weaken the keyless-loopback-only consent law: `keyless`
// is prefilled true only for the two loopback presets, and the backend
// validation (`ConnectionError::KeylessRequiresLoopback`) re-checks every
// create/update/draft regardless of what the form sends.

export interface OpenAiCompatPreset {
  id: string;
  label: string;
  baseUrl: string;
  /// Where the provider documents retention/handling of what you send;
  /// null for loopback servers — nothing leaves this computer.
  dataHandlingUrl: string | null;
  /// Loopback server that lawfully runs without a key.
  keyless: boolean;
}

export const OPENAI_COMPAT_PRESETS: OpenAiCompatPreset[] = [
  // https://docs.ollama.com/openai (checked 2026-08-29): base
  // http://localhost:11434/v1, supports GET /v1/models.
  {
    id: "ollama",
    label: "Ollama (this computer)",
    baseUrl: "http://localhost:11434/v1",
    dataHandlingUrl: null,
    keyless: true,
  },
  // https://lmstudio.ai/docs/app/api/endpoints/openai (checked
  // 2026-08-29): base http://localhost:1234/v1, GET /v1/models supported.
  {
    id: "lmstudio",
    label: "LM Studio (this computer)",
    baseUrl: "http://localhost:1234/v1",
    dataHandlingUrl: null,
    keyless: true,
  },
  // https://developers.openai.com/api/docs/guides/your-data (checked
  // 2026-08-29) is OpenAI's API data-handling guide; the adapter already
  // pins api.openai.com/v1 as the documented canonical host and GET
  // /v1/models answers live (401 without a key = endpoint present).
  {
    id: "openai",
    label: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    dataHandlingUrl: "https://developers.openai.com/api/docs/guides/your-data",
    keyless: false,
  },
  // https://openrouter.ai/docs/quickstart (checked 2026-08-29): base
  // https://openrouter.ai/api/v1, GET /api/v1/models supported. Data
  // handling: https://openrouter.ai/docs/features/privacy-and-logging.
  {
    id: "openrouter",
    label: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    dataHandlingUrl: "https://openrouter.ai/docs/features/privacy-and-logging",
    keyless: false,
  },
  // https://console.groq.com/docs/openai (checked 2026-08-29): base
  // https://api.groq.com/openai/v1. Privacy: https://groq.com/privacy-policy.
  {
    id: "groq",
    label: "Groq",
    baseUrl: "https://api.groq.com/openai/v1",
    dataHandlingUrl: "https://groq.com/privacy-policy",
    keyless: false,
  },
  // https://docs.together.ai/docs/openai-api-compatibility (checked
  // 2026-08-29): base https://api.together.ai/v1. Privacy:
  // https://www.together.ai/privacy.
  {
    id: "together",
    label: "Together AI",
    baseUrl: "https://api.together.ai/v1",
    dataHandlingUrl: "https://www.together.ai/privacy",
    keyless: false,
  },
  // https://docs.mistral.ai/api (checked 2026-08-29): base
  // https://api.mistral.ai/v1, list_models_v1_models_get. Terms hub
  // https://mistral.ai/terms 301s to legal.mistral.ai (privacy policy
  // lives there; the redirect was the live-verifiable fact this session).
  {
    id: "mistral",
    label: "Mistral",
    baseUrl: "https://api.mistral.ai/v1",
    dataHandlingUrl: "https://mistral.ai/terms",
    keyless: false,
  },
  // https://api-docs.deepseek.com (checked 2026-08-29): base
  // https://api.deepseek.com (no /v1 in the documented examples), and
  // https://api-docs.deepseek.com/api/list-models documents GET /models.
  // Privacy: the policy PDF host under cdn.deepseek.com.
  {
    id: "deepseek",
    label: "DeepSeek",
    baseUrl: "https://api.deepseek.com",
    dataHandlingUrl:
      "https://cdn.deepseek.com/policies/en-US/deepseek-privacy-policy.html",
    keyless: false,
  },
];

export const CUSTOM_PRESET_ID = "custom";

export function presetById(id: string): OpenAiCompatPreset | null {
  return OPENAI_COMPAT_PRESETS.find((preset) => preset.id === id) ?? null;
}
