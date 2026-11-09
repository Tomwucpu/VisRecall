import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import { api, type ModelConfigDto, type ProbeOutcomeDto } from "../../lib/api";
import { errorMessage } from "../../lib/errors";
import { queryKeys } from "../../lib/queryClient";
import {
  emptyDraft,
  fromConfig,
  newModelId,
  protocolLabel,
  toInput,
  validateDraft,
  type ModelDraft,
  type ModelSnapshot,
} from "../../lib/modelConfig";
import { pushToast } from "../../lib/toast";
import { ModelForm } from "./ModelForm";

type Editor = { mode: "new" | "edit"; draft: ModelDraft } | null;

/** 探测结果的三态文案（成功 / 失败 / 未探测）。 */
function probeText(probe: ProbeOutcomeDto | null, probing: boolean): string {
  if (probing) {
    return "测试中…";
  }
  if (probe === null) {
    return "";
  }
  if (!probe.ok) {
    return `测试失败：${probe.message ?? "连接不可用"}`;
  }
  return `连通 ${probe.latencyMs}ms · ${probe.vision ? "视觉可用" : "仅文本"}`;
}

/**
 * 「模型」pane：原型式的自定义模型列表 + 新建/修改表单。
 *
 * 密钥纪律：密码框只写不读——后端只回 `hasApiKey`，整表保存成功后由后端视图
 * 重建草稿，界面任何时刻都不回显密钥（包括错误信息与日志）。
 */
export function ModelSettingsPane() {
  const queryClient = useQueryClient();
  const config = useQuery({
    queryKey: queryKeys.modelConfig,
    queryFn: api.getModelConfig,
  });

  // `null` = 尚未本地编辑；之后以后端返回的整表为准，避免编辑中被重取覆盖。
  const [snapshot, setSnapshot] = useState<ModelSnapshot | null>(null);
  const [editor, setEditor] = useState<Editor>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [probe, setProbe] = useState<ProbeOutcomeDto | null>(null);
  const [probing, setProbing] = useState(false);
  const [busy, setBusy] = useState(false);

  const view: ModelSnapshot = snapshot ?? fromConfig(config.data);
  const active =
    view.entries.find((entry) => entry.id === view.activeId) ?? view.entries[0] ?? null;
  // 配置到达前列表本来就是空的，卡片类操作无从触发；只需在写入期间锁住按钮。
  const locked = busy;

  /** 整表提交：新建 / 修改 / 删除 / 选用都走同一条命令，返回后端视图。 */
  const commit = async (
    entries: ModelDraft[],
    activeId: string | null,
    toast?: string,
  ): Promise<ModelConfigDto | null> => {
    setBusy(true);
    try {
      const saved = await api.setModelConfig({
        entries: entries.map(toInput),
        activeId,
      });
      setSnapshot(fromConfig(saved));
      queryClient.setQueryData(queryKeys.modelConfig, saved);
      if (toast !== undefined) {
        pushToast(toast);
      }
      return saved;
    } catch (error) {
      pushToast(errorMessage(error), "error");
      return null;
    } finally {
      setBusy(false);
    }
  };

  const openNew = () => {
    setEditor({ mode: "new", draft: { ...emptyDraft(), id: newModelId() } });
    setFormError(null);
    setProbe(null);
  };

  const openEdit = (entry: ModelDraft) => {
    setEditor({
      mode: "edit",
      draft: { ...entry, apiKey: "", clearApiKey: false },
    });
    setFormError(null);
    setProbe(null);
  };

  const saveEditor = async () => {
    if (editor === null || locked) {
      return;
    }
    const problem = validateDraft(editor.draft);
    if (problem !== null) {
      setFormError(problem);
      return;
    }
    const draft: ModelDraft = {
      ...editor.draft,
      name: editor.draft.name.trim(),
      model: editor.draft.model.trim(),
      baseUrl: editor.draft.baseUrl.trim(),
      protocolId: editor.draft.protocolId.trim(),
    };
    const exists = view.entries.some((entry) => entry.id === draft.id);
    const next = exists
      ? view.entries.map((entry) => (entry.id === draft.id ? draft : entry))
      : [...view.entries, draft];
    const nextActive = editor.mode === "new" ? draft.id : (view.activeId ?? draft.id);
    const label = editor.mode === "new" ? "已保存并启用" : "已保存修改";
    const saved = await commit(next, nextActive, `${label}：${draft.name}`);
    if (saved !== null) {
      setEditor(null);
      setFormError(null);
      setProbe(null);
    }
  };

  const selectModel = async (id: string) => {
    if (locked || id === view.activeId) {
      return;
    }
    const target = view.entries.find((entry) => entry.id === id);
    const saved = await commit(view.entries, id, `已选用 ${target?.name ?? id}`);
    if (saved !== null) {
      setProbe(null);
    }
  };

  const removeModel = async (id: string) => {
    if (locked) {
      return;
    }
    const target = view.entries.find((entry) => entry.id === id);
    const next = view.entries.filter((entry) => entry.id !== id);
    const nextActive = view.activeId === id ? (next[0]?.id ?? null) : view.activeId;
    const saved = await commit(next, nextActive, `已删除模型：${target?.name ?? id}`);
    if (saved !== null) {
      setProbe(null);
      if (editor?.draft.id === id) {
        setEditor(null);
        setFormError(null);
      }
    }
  };

  const testConnection = async () => {
    if (active === null) {
      setProbe({
        ok: false,
        latencyMs: 0,
        vision: false,
        message: "请先添加并保存一个模型",
      });
      return;
    }
    setProbing(true);
    setProbe(null);
    try {
      setProbe(await api.probeModel(active.id));
    } catch (error) {
      setProbe({
        ok: false,
        latencyMs: 0,
        vision: false,
        message: errorMessage(error),
      });
    } finally {
      setProbing(false);
    }
  };

  const text = probeText(probe, probing);

  return (
    <div className="pane">
      <p className="pane-desc">
        不预置任何厂商模型。每个模型由你定义名称、ID、接口地址、通信协议与密钥。点击卡片即可选用，右侧按钮用于修改或删除。
        摘要与标签由云端 VLM 生成，解析、字面检索与嵌入完全本地；密钥只写入 Windows 凭据管理器，界面永不回显。
      </p>

      <div className={`mc-head${editor !== null ? " mc-hidden" : ""}`}>
        <span className="label">
          已配置模型<span className="mono">({view.entries.length})</span>
        </span>
        <button type="button" className="mini-btn" disabled={locked} onClick={openNew}>
          + 新建模型
        </button>
      </div>

      <div className={`model-cards${editor !== null ? " mc-hidden" : ""}`}>
        {view.entries.length === 0 ? (
          <div className="mc-empty">还没有模型。点右上角「+ 新建模型」添加第一个配置。</div>
        ) : (
          view.entries.map((entry) => {
            const isActive = entry.id === active?.id;
            const meta = [
              entry.model,
              protocolLabel(entry.protocol),
              entry.baseUrl || "未填接口",
              entry.hasApiKey ? "密钥已配置" : "无密钥",
            ].join(" · ");
            return (
              <div
                key={entry.id}
                className={`mc${isActive ? " on" : ""}`}
                role="button"
                tabIndex={0}
                aria-pressed={isActive}
                title={isActive ? "当前使用中的模型" : "点击选用此模型"}
                onClick={() => void selectModel(entry.id)}
                onKeyDown={(event) => {
                  if (event.target !== event.currentTarget) {
                    return;
                  }
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    void selectModel(entry.id);
                  }
                }}
              >
                <div className="mc-main">
                  <span className="mc-dot" />
                  <div className="mc-info">
                    <div className="mc-name">
                      {entry.name}
                      {isActive && <span className="mc-active">使用中</span>}
                    </div>
                    <div className="mc-meta mono">{meta}</div>
                  </div>
                </div>
                <div className="mc-ops">
                  <button
                    type="button"
                    className="mini-btn"
                    disabled={locked}
                    onClick={(event) => {
                      event.stopPropagation();
                      openEdit(entry);
                    }}
                  >
                    修改
                  </button>
                  <button
                    type="button"
                    className="mini-btn"
                    disabled={locked}
                    onClick={(event) => {
                      event.stopPropagation();
                      void removeModel(entry.id);
                    }}
                  >
                    删除
                  </button>
                </div>
              </div>
            );
          })
        )}
      </div>

      {editor !== null && (
        <ModelForm
          mode={editor.mode}
          draft={editor.draft}
          error={formError}
          busy={busy}
          onChange={(patch) => {
            setEditor({ ...editor, draft: { ...editor.draft, ...patch } });
            setFormError(null);
          }}
          onCancel={() => {
            setEditor(null);
            setFormError(null);
          }}
          onSave={() => void saveEditor()}
        />
      )}

      <div className="row">
        <div>
          <div className="row-t">当前模型</div>
          <div className="row-d">
            {active === null
              ? "未配置模型 · 仅本地 OCR，语义描述排队等待"
              : `${active.model} · ${protocolLabel(active.protocol)} · 密钥${active.hasApiKey ? "已配置" : "未配置"}`}
          </div>
        </div>
        <div className="row-r">
          <button
            type="button"
            className="btn"
            disabled={locked || probing || active === null}
            onClick={() => void testConnection()}
          >
            {probing ? "测试中…" : "测试连接"}
          </button>
        </div>
      </div>
      {text !== "" && (
        <p className={`model-probe${probe !== null && !probe.ok ? " is-error" : ""}`} role="status">
          {text}
        </p>
      )}

      <div className="row">
        <div>
          <div className="row-t">本地模型模式</div>
          <div className="row-d">完全离线运行，质量下降但数据不出本机。V1.0 提供。</div>
        </div>
        <div className="row-r">
          <button
            type="button"
            className="switch"
            disabled
            title="V1.0 提供"
            aria-label="本地模型模式（V1.0 提供）"
          >
            <i />
          </button>
        </div>
      </div>
    </div>
  );
}
