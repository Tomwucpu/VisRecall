import { useState } from "react";
import { useQuery } from "@tanstack/react-query";

import { api, type ModelConfigDto, type ProbeOutcomeDto } from "../../lib/api";
import { errorMessage } from "../../lib/errors";
import { queryKeys } from "../../lib/queryClient";
import { isValidBaseUrl, type SlotDraft } from "../../lib/modelConfig";
import { pushToast } from "../../lib/toast";
import { ModelSlotForm } from "./ModelSlotForm";

type Slot = "primary" | "backup";

const SLOT_LABEL: Record<Slot, string> = {
  primary: "主模型",
  backup: "备用模型",
};

function toDraft(
  config: ModelConfigDto | undefined,
  slot: Slot,
): SlotDraft {
  const entry = slot === "primary" ? config?.primary : config?.backup;
  return {
    baseUrl: entry?.baseUrl ?? "",
    model: entry?.model ?? "",
    apiKey: "",
    hasApiKey: entry?.hasApiKey ?? false,
  };
}

/**
 * 「模型」pane（P3-R11）。
 *
 * 密钥纪律：密码框只写不读——后端只回 `hasApiKey`，保存成功后立即清空输入框，
 * 界面任何时刻都不回显密钥（包括错误信息与日志）。
 */
export function ModelSettingsPane() {
  const config = useQuery({
    queryKey: queryKeys.modelConfig,
    queryFn: api.getModelConfig,
  });

  const [drafts, setDrafts] = useState<Record<Slot, SlotDraft> | null>(null);
  const [errors, setErrors] = useState<Record<Slot, string | null>>({
    primary: null,
    backup: null,
  });
  const [probes, setProbes] = useState<Record<Slot, ProbeOutcomeDto | null>>({
    primary: null,
    backup: null,
  });
  const [probing, setProbing] = useState<Record<Slot, boolean>>({
    primary: false,
    backup: false,
  });
  const [saving, setSaving] = useState(false);

  // 配置到达前用空草稿；之后以本地草稿为准（用户正在编辑时不被重取覆盖）
  const current: Record<Slot, SlotDraft> =
    drafts ?? {
      primary: toDraft(config.data, "primary"),
      backup: toDraft(config.data, "backup"),
    };

  const update = (slot: Slot, patch: Partial<SlotDraft>) => {
    setDrafts({
      ...current,
      [slot]: { ...current[slot], ...patch },
    });
  };

  const validate = (slot: Slot): boolean => {
    const draft = current[slot];
    if (draft.baseUrl.trim().length === 0 && draft.model.trim().length === 0) {
      return true; // 允许清空槽位
    }
    if (!isValidBaseUrl(draft.baseUrl)) {
      setErrors((prev) => ({
        ...prev,
        [slot]: "地址必须以 http:// 或 https:// 开头",
      }));
      return false;
    }
    if (draft.model.trim().length === 0) {
      setErrors((prev) => ({ ...prev, [slot]: "请填写模型名" }));
      return false;
    }
    setErrors((prev) => ({ ...prev, [slot]: null }));
    return true;
  };

  const save = async () => {
    if (!validate("primary") || !validate("backup")) {
      return;
    }
    setSaving(true);
    try {
      const saved = await api.setModelConfig({
        primary: {
          baseUrl: current.primary.baseUrl,
          model: current.primary.model,
          apiKey: current.primary.apiKey || undefined,
        },
        backup: {
          baseUrl: current.backup.baseUrl,
          model: current.backup.model,
          apiKey: current.backup.apiKey || undefined,
        },
      });
      // 保存成功后密钥输入框必须清空，且不把密钥写回任何 state
      setDrafts({
        primary: { ...toDraft(saved, "primary"), apiKey: "" },
        backup: { ...toDraft(saved, "backup"), apiKey: "" },
      });
      pushToast("模型配置已保存，立即生效");
    } catch (error) {
      pushToast(errorMessage(error), "error");
    } finally {
      setSaving(false);
    }
  };

  const probe = async (slot: Slot) => {
    if (!validate(slot)) {
      return;
    }
    // 探测必须用**已保存**的配置：未保存的草稿不生效于后端，先提示用户保存
    const saved = slot === "primary" ? config.data?.primary : config.data?.backup;
    if (saved === undefined || saved === null) {
      setProbes((prev) => ({
        ...prev,
        [slot]: {
          ok: false,
          latencyMs: 0,
          vision: false,
          message: "请先保存该槽位再探测",
        },
      }));
      return;
    }
    setProbing((prev) => ({ ...prev, [slot]: true }));
    try {
      const outcome = await api.probeModel(slot);
      setProbes((prev) => ({ ...prev, [slot]: outcome }));
    } catch (error) {
      setProbes((prev) => ({
        ...prev,
        [slot]: {
          ok: false,
          latencyMs: 0,
          vision: false,
          message: errorMessage(error),
        },
      }));
    } finally {
      setProbing((prev) => ({ ...prev, [slot]: false }));
    }
  };

  return (
    <div className="pane">
      <p className="pane-desc">
        阶段二的摘要与标签由云端 VLM 生成。查询路径（解析、字面检索、嵌入）完全本地，不联网。
        密钥只写入 Windows 凭据管理器，界面永不回显。
      </p>

      {(["primary", "backup"] as Slot[]).map((slot) => (
        <ModelSlotForm
          key={slot}
          label={SLOT_LABEL[slot]}
          draft={current[slot]}
          error={errors[slot]}
          probe={probes[slot]}
          probing={probing[slot]}
          onChange={(patch) => update(slot, patch)}
          onProbe={() => void probe(slot)}
        />
      ))}

      <div className="row-r" style={{ marginTop: 12 }}>
        <button
          type="button"
          className="btn primary"
          disabled={saving || config.isPending}
          onClick={() => void save()}
        >
          {saving ? "保存中…" : "保存配置"}
        </button>
      </div>
      <p className="row-d">
        留空地址与模型名可清空该槽位；密钥框留空表示不修改，保存后自动清空。
      </p>
    </div>
  );
}
