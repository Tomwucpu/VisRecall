import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { beforeEach, test, vi } from "vitest";

import { api, type ModelConfigDto } from "../../lib/api";
import { ModelSettingsPane } from "./ModelSettingsPane";

const EMPTY: ModelConfigDto = { primary: null, backup: null };

const CONFIGURED: ModelConfigDto = {
  primary: {
    baseUrl: "https://api.example.com/v1",
    model: "gpt-4o-mini",
    hasApiKey: true,
  },
  backup: null,
};

function renderPane() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <ModelSettingsPane />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.restoreAllMocks();
});

test("shows two slots and only a boolean key badge, never the secret", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(CONFIGURED);
  renderPane();

  expect(await screen.findByText("已设置")).toBeInTheDocument();
  expect(screen.getByLabelText("主模型接口地址")).toHaveValue("https://api.example.com/v1");
  expect(screen.getByLabelText("主模型模型名")).toHaveValue("gpt-4o-mini");
  expect(screen.getByLabelText("主模型 API Key")).toHaveValue("");
  expect(screen.getAllByLabelText(/API Key/)).toHaveLength(2);
  expect(screen.queryByText(/sk-/)).not.toBeInTheDocument();
});

test("invalid base url shows an inline error and does not save", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(EMPTY);
  const save = vi.spyOn(api, "setModelConfig");
  renderPane();
  await screen.findByText("保存配置");

  fireEvent.change(screen.getByLabelText("主模型接口地址"), {
    target: { value: "api.example.com" },
  });
  fireEvent.change(screen.getByLabelText("主模型模型名"), {
    target: { value: "m" },
  });
  fireEvent.click(screen.getByText("保存配置"));

  expect(await screen.findByRole("alert")).toHaveTextContent("http://");
  expect(save).not.toHaveBeenCalled();
});

test("saving clears the password box and sends no plaintext back into the form", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(EMPTY);
  const save = vi.spyOn(api, "setModelConfig").mockImplementation(async (patch) => {
    // 后端视图只回 hasApiKey，绝不回传密钥
    expect(patch.primary?.apiKey).toBe("sk-secret");
    return {
      primary: {
        baseUrl: "https://api.example.com/v1",
        model: "m",
        hasApiKey: true,
      },
      backup: null,
    };
  });

  renderPane();
  await screen.findByText("保存配置");
  fireEvent.change(screen.getByLabelText("主模型接口地址"), {
    target: { value: "https://api.example.com/v1" },
  });
  fireEvent.change(screen.getByLabelText("主模型模型名"), {
    target: { value: "m" },
  });
  const keyInput = screen.getByLabelText("主模型 API Key") as HTMLInputElement;
  fireEvent.change(keyInput, { target: { value: "sk-secret" } });
  fireEvent.click(screen.getByText("保存配置"));

  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  await waitFor(() => expect(keyInput).toHaveValue(""));
  expect(await screen.findByText("已设置")).toBeInTheDocument();
  expect(document.body.textContent).not.toContain("sk-secret");
});
test("probe on an unsaved slot asks to save first", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(EMPTY);
  const probe = vi.spyOn(api, "probeModel");
  renderPane();
  await screen.findByText("保存配置");

  fireEvent.click(screen.getAllByText("探测")[0]);
  expect(await screen.findByText("探测失败：请先保存该槽位再探测")).toBeInTheDocument();
  expect(probe).not.toHaveBeenCalled();
});

test("probe reports success and failure states", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(CONFIGURED);
  const probe = vi
    .spyOn(api, "probeModel")
    .mockResolvedValueOnce({ ok: true, latencyMs: 42, vision: true, message: null })
    .mockRejectedValueOnce({ code: "PROVIDER_CONFIG", message: "HTTP 401" });
  renderPane();
  await screen.findByText("已设置");

  const buttons = screen.getAllByText("探测");
  fireEvent.click(buttons[0]);
  expect(await screen.findByText(/连通 42ms · 视觉可用/)).toBeInTheDocument();
  expect(probe).toHaveBeenCalledWith("primary");

  fireEvent.click(buttons[0]);
  expect(await screen.findByText("探测失败：HTTP 401")).toBeInTheDocument();
});