import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, test, vi } from "vitest";

import {
  api,
  type ModelConfigDto,
  type ModelConfigPatchDto,
  type SettingsDto,
} from "../../lib/api";
import { resetToasts } from "../../lib/toast";
import { ToastViewport } from "../shell/ToastViewport";
import { ModelSettingsPane } from "./ModelSettingsPane";

const EMPTY: ModelConfigDto = { entries: [], activeId: null };

/** 设置 DTO 的完整形状（新增字段时这里必须一起改，避免用例悄悄失效）。 */
function settings(cloudTextAi: boolean, cloudImageAi = false): SettingsDto {
  return {
    libraryPath: "D:\\VisRecall\\library",
    captureHotkey: "Ctrl+Shift+S",
    ballEnabled: true,
    ballSize: 48,
    ballEdgeHide: true,
    hideInFullscreen: true,
    capacityGb: 50,
    retentionDays: null,
    cloudImageAi,
    cloudTextAi,
  };
}

const CONFIGURED: ModelConfigDto = {
  entries: [
    {
      id: "m1",
      name: "本地 VL 模型",
      model: "qwen2.5-vl-7b",
      baseUrl: "http://localhost:11434/v1",
      protocol: "ollama",
      protocolId: "",
      hasApiKey: true,
    },
    {
      id: "m2",
      name: "自研协议接口",
      model: "my-vl-v2",
      baseUrl: "https://api.example.com/v1",
      protocol: "rest",
      protocolId: "my-proto/v1",
      hasApiKey: false,
    },
  ],
  activeId: "m1",
};

function renderPane() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <ModelSettingsPane />
      <ToastViewport />
    </QueryClientProvider>,
  );
}

/** 后端视图：只回 hasApiKey，绝不回明文密钥。 */
function echoConfig(patch: ModelConfigPatchDto): ModelConfigDto {
  return {
    entries: patch.entries.map((entry) => ({
      id: entry.id,
      name: entry.name,
      model: entry.model,
      baseUrl: entry.baseUrl,
      protocol: entry.protocol,
      protocolId: entry.protocolId,
      hasApiKey: entry.apiKey !== null && entry.apiKey !== undefined && entry.apiKey !== "",
    })),
    activeId: patch.activeId ?? null,
  };
}

async function openNewForm() {
  fireEvent.click(await screen.findByText("+ 新建模型"));
  fireEvent.change(screen.getByLabelText("显示名称"), { target: { value: "我的视觉模型" } });
  fireEvent.change(screen.getByLabelText("模型 ID"), {
    target: { value: "qwen2.5-vl-72b" },
  });
  fireEvent.change(screen.getByLabelText("接口地址 / Base URL"), {
    target: { value: "https://api.example.com/v1" },
  });
}

beforeEach(() => {
  vi.restoreAllMocks();
  // 默认每个用例都有可用的设置快照（开关关闭），用例可再覆盖。
  vi.spyOn(api, "getSettings").mockResolvedValue(settings(false));
});

afterEach(() => {
  resetToasts();
});

test("renders the model cards with only a boolean key state, never the secret", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(CONFIGURED);
  renderPane();

  expect(await screen.findByText("本地 VL 模型")).toBeInTheDocument();
  expect(screen.getByText("自研协议接口")).toBeInTheDocument();
  expect(screen.getByText("使用中")).toBeInTheDocument();
  const card = screen.getByText("本地 VL 模型").closest(".mc");
  expect(card).toHaveTextContent("密钥已配置");
  expect(document.body.textContent).not.toContain("sk-");
});

test("creating a model sends the whole list and clears the password box", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(EMPTY);
  const save = vi
    .spyOn(api, "setModelConfig")
    .mockImplementation(async (patch) => echoConfig(patch));

  renderPane();
  await openNewForm();
  const keyInput = screen.getByLabelText("API 密钥") as HTMLInputElement;
  fireEvent.change(keyInput, { target: { value: "sk-secret" } });
  fireEvent.click(screen.getByText("保存并启用"));

  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  const patch = save.mock.calls[0][0];
  expect(patch.entries).toHaveLength(1);
  expect(patch.entries[0].name).toBe("我的视觉模型");
  expect(patch.entries[0].model).toBe("qwen2.5-vl-72b");
  expect(patch.entries[0].protocol).toBe("openai");
  expect(patch.entries[0].apiKey).toBe("sk-secret");
  expect(patch.activeId).toBe(patch.entries[0].id);

  expect(await screen.findByText("我的视觉模型")).toBeInTheDocument();
  expect(screen.queryByLabelText("API 密钥")).not.toBeInTheDocument();
  expect(document.body.textContent).not.toContain("sk-secret");
});

test("invalid base url shows an inline error and does not save", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(EMPTY);
  const save = vi.spyOn(api, "setModelConfig");
  renderPane();

  await openNewForm();
  fireEvent.change(screen.getByLabelText("接口地址 / Base URL"), {
    target: { value: "api.example.com" },
  });
  fireEvent.click(screen.getByText("保存并启用"));

  expect(await screen.findByRole("alert")).toHaveTextContent("http://");
  expect(save).not.toHaveBeenCalled();
});

test("protocol menu writes the protocol and custom REST requires an id", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(EMPTY);
  const save = vi
    .spyOn(api, "setModelConfig")
    .mockImplementation(async (patch) => echoConfig(patch));
  renderPane();

  await openNewForm();
  fireEvent.click(screen.getByRole("button", { name: /OpenAI 兼容/ }));
  fireEvent.click(screen.getByRole("menuitemradio", { name: /自定义 REST/ }));
  expect(screen.getByLabelText("协议标识")).toBeInTheDocument();

  fireEvent.click(screen.getByText("保存并启用"));
  expect(await screen.findByRole("alert")).toHaveTextContent("协议标识");
  expect(save).not.toHaveBeenCalled();

  fireEvent.change(screen.getByLabelText("协议标识"), {
    target: { value: "my-proto/v1" },
  });
  fireEvent.click(screen.getByText("保存并启用"));
  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  expect(save.mock.calls[0][0].entries[0].protocol).toBe("rest");
  expect(save.mock.calls[0][0].entries[0].protocolId).toBe("my-proto/v1");
});

test("clearing a saved key sends an empty string, not plaintext", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(CONFIGURED);
  const save = vi
    .spyOn(api, "setModelConfig")
    .mockImplementation(async (patch) => echoConfig(patch));
  renderPane();

  expect(await screen.findByText("本地 VL 模型")).toBeInTheDocument();
  fireEvent.click(screen.getAllByText("修改")[0]);
  fireEvent.click(screen.getByText("清除已保存密钥"));
  fireEvent.click(screen.getByText("保存修改"));

  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  expect(save.mock.calls[0][0].entries[0].apiKey).toBe("");
  expect(save.mock.calls[0][0].entries[1].apiKey).toBeNull();
});

test("selecting and deleting a model persists the whole list", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(CONFIGURED);
  const save = vi
    .spyOn(api, "setModelConfig")
    .mockImplementation(async (patch) => echoConfig(patch));
  renderPane();

  expect(await screen.findByText("本地 VL 模型")).toBeInTheDocument();
  fireEvent.click(screen.getByText("自研协议接口"));
  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  expect(save.mock.calls[0][0].activeId).toBe("m2");

  fireEvent.click(screen.getAllByText("删除")[0]);
  await waitFor(() => expect(save).toHaveBeenCalledTimes(2));
  expect(save.mock.calls[1][0].entries.map((entry) => entry.id)).toEqual(["m2"]);
});

test("test connection reports success and failure states", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(CONFIGURED);
  const probe = vi
    .spyOn(api, "probeModel")
    .mockResolvedValueOnce({ ok: true, latencyMs: 42, vision: true, message: null })
    .mockRejectedValueOnce({ code: "PROVIDER_CONFIG", message: "HTTP 401" });
  renderPane();

  expect(await screen.findByText("本地 VL 模型")).toBeInTheDocument();
  fireEvent.click(screen.getByText("测试连接"));
  expect(await screen.findByText(/连通 42ms · 视觉可用/)).toBeInTheDocument();
  expect(probe).toHaveBeenCalledWith("m1");

  fireEvent.click(screen.getByText("测试连接"));
  expect(await screen.findByText("测试失败：HTTP 401")).toBeInTheDocument();
});

test("cloud image ai switch is off by default and persists a toggle", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(EMPTY);
  let enabled = false;
  vi.spyOn(api, "getSettings").mockImplementation(async () => settings(false, enabled));
  const save = vi.spyOn(api, "setCloudImageAi").mockImplementation(async (next) => {
    enabled = next;
  });
  renderPane();

  const toggle = await screen.findByRole("switch", { name: "图片上传云端解析" });
  expect(toggle).toHaveAttribute("aria-checked", "false");
  await waitFor(() => expect(toggle).toBeEnabled());

  fireEvent.click(toggle);
  await waitFor(() => expect(save).toHaveBeenCalledWith(true));
  await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"));
});

test("cloud image ai toggle failure keeps the old state and reports an error", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(EMPTY);
  vi.spyOn(api, "setCloudImageAi").mockRejectedValue({
    code: "DATABASE",
    message: "database is locked",
  });
  renderPane();

  const toggle = await screen.findByRole("switch", { name: "图片上传云端解析" });
  await waitFor(() => expect(toggle).toBeEnabled());
  fireEvent.click(toggle);

  expect(await screen.findByText(/保存失败/)).toBeInTheDocument();
  expect(toggle).toHaveAttribute("aria-checked", "false");
});

test("cloud text ai switch is off by default and persists a toggle", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(EMPTY);
  let enabled = false;
  vi.spyOn(api, "getSettings").mockImplementation(async () => settings(enabled));
  const save = vi.spyOn(api, "setCloudTextAi").mockImplementation(async (next) => {
    enabled = next;
  });
  renderPane();

  const toggle = await screen.findByRole("switch", { name: "文本上传云端解析" });
  expect(toggle).toHaveAttribute("aria-checked", "false");
  // 设置到达前开关是禁用的（避免把「未读到」当成「关闭」写回去）。
  await waitFor(() => expect(toggle).toBeEnabled());

  fireEvent.click(toggle);
  await waitFor(() => expect(save).toHaveBeenCalledWith(true));
  // 保存后重新读取设置，开关跟着落盘值走
  await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"));
});

test("cloud text ai toggle failure keeps the old state and reports an error", async () => {
  vi.spyOn(api, "getModelConfig").mockResolvedValue(EMPTY);
  vi.spyOn(api, "setCloudTextAi").mockRejectedValue({
    code: "DATABASE",
    message: "database is locked",
  });
  renderPane();

  const toggle = await screen.findByRole("switch", { name: "文本上传云端解析" });
  await waitFor(() => expect(toggle).toBeEnabled());
  fireEvent.click(toggle);

  expect(await screen.findByText(/保存失败/)).toBeInTheDocument();
  expect(toggle).toHaveAttribute("aria-checked", "false");
});
