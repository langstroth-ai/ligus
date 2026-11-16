import { describe, it, expect } from "vitest";
import { formOllamaListEnabled } from "./ModelsSection";

describe("formOllamaListEnabled", () => {
  const base = { provider: "ollama", endpoint: "", ollamaConfigured: false, asked: false };
  it("never probes localhost for someone who never set up Ollama", () => {
    expect(formOllamaListEnabled(base)).toBe(false);
  });
  it("lists once there is a server to ask", () => {
    expect(formOllamaListEnabled({ ...base, endpoint: "http://gpu-box:11434" })).toBe(true);
    expect(formOllamaListEnabled({ ...base, ollamaConfigured: true })).toBe(true);
    expect(formOllamaListEnabled({ ...base, asked: true })).toBe(true);
  });
  it("only for the Ollama provider", () => {
    expect(formOllamaListEnabled({ ...base, provider: "anthropic", asked: true })).toBe(false);
  });
});
