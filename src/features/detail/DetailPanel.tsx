import { useEffect, useRef, useState, type CSSProperties } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, SUMMARY_PLACEHOLDER, type TagChip } from "../../lib/api";
import { errorCode, errorMessage } from "../../lib/errors";
import {
  copyLabel,
  formatAbsoluteTime,
  formatRelativeTime,
  kindLabel,
  statusLabel,
  toOriginalSrc,
} from "../../lib/format";
import { queryKeys } from "../../lib/queryClient";
import { pushToast } from "../../lib/toast";
import { ConfirmDialog } from "../common/ConfirmDialog";
import type { ItemCardData } from "../library/ItemCard";
import { displayTitle } from "../../lib/itemText";
import { ImageOverlay } from "./ImageOverlay";

type Props = {
  item: ItemCardData;
  onClose: () => void;
  /** 打开重命名弹窗；不传时标题区不显示铅笔按钮。 */
  onRename?: (item: ItemCardData) => void;
};/** 备注上限（契约草案：10000 字，超限阻止输入而不是截断保存）。 */
const NOTE_LIMIT = 10_000;

/** AI 描述上限（与后端 `MAX_SUMMARY_CHARS` 对齐）。 */
const SUMMARY_LIMIT = 2_000;

/** 正文与 OCR 上限（与后端 `MAX_BODY_CHARS` / `MAX_OCR_CHARS` 对齐）。 */
const CONTENT_LIMIT = 20_000;

/**
 * 按 Unicode 码点截断：`String.length` 是 UTF-16 单元，emoji 会被算成 2，
 * 而后端按 `chars().count()` 计数，两边口径必须一致。
 */
function clampToChars(value: string, limit: number): string {
  const chars = Array.from(value);
  return chars.length > limit ? chars.slice(0, limit).join("") : value;
}

/** 编辑区状态文案：与备注四态一致。 */
function saveStateMeta(state: SaveState, dirty: boolean): string {
  if (state === "saving") {
    return "保存中…";
  }
  if (state === "error") {
    return "保存失败 · 文本未丢失";
  }
  if (state === "saved") {
    return "已保存";
  }
  return dirty ? "未保存" : "";
}

type SaveState = "idle" | "saving" | "saved" | "error";

/** `null` 关闭；`plain` 只看原图；`boxes` 额外叠加 OCR 文字高亮。 */
type OverlayMode = "plain" | "boxes" | null;

/** 摘要来源 → 徽标文案（与后端 `summary_source` 的取值一一对应）。 */
const SUMMARY_SOURCE_LABEL: Record<string, string> = {
  vlm: "VLM",
  ocr: "OCR 摘录",
  extractive: "原文摘录",
  user: "已编辑",
};

const REFERENCE_STATUS_LABEL: Record<string, string> = {
  available: "可用",
  missing: "文件缺失",
  permission_denied: "无访问权限",
  changed: "文件已变化",
  removed: "来源已移除",
};

/**
 * 详情面板（T4）。
 *
 * 数据分层：基础字段来自真实 `get_item`，AI 描述 / 原文 / 原图 / 标签走 `api` stub，
 * 缺能力处一律显式占位（「待阶段 02 提供」），不显示任何伪造内容。
 * AI 描述区块只渲染 `SUMMARY_PLACEHOLDER`，复制按钮始终禁用（红线）。
 */
export function DetailPanel({ item, onClose, onRename }: Props) {
  const queryClient = useQueryClient();
  const detail = useQuery({
    queryKey: queryKeys.detail(item.id),
    queryFn: () => api.loadDetail(item.id),
  });

  const [note, setNote] = useState<string | null>(null);
  /** 已落盘的备注：点「取消」回到它，保存成功后同步成新值。 */
  const [savedNote, setSavedNote] = useState("");
  /** 备注区（输入框 + 按钮）是否获得焦点：用于「没在编辑且没改动」时收起按钮。 */
  const [noteFocused, setNoteFocused] = useState(false);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [tagDraft, setTagDraft] = useState<string | null>(null);
  const [overlayMode, setOverlayMode] = useState<OverlayMode>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [reparseBusy, setReparseBusy] = useState(false);
  const [referenceBusy, setReferenceBusy] = useState(false);

  // 三块可编辑内容各自维护草稿 / 已保存快照 / 保存态；`null` 草稿表示处于只读态。
  const [summaryDraft, setSummaryDraft] = useState<string | null>(null);
  const [summarySaved, setSummarySaved] = useState("");
  const [summaryState, setSummaryState] = useState<SaveState>("idle");
  const [bodyDraft, setBodyDraft] = useState<string | null>(null);
  const [bodySaved, setBodySaved] = useState("");
  const [bodyState, setBodyState] = useState<SaveState>("idle");
  const [ocrDraft, setOcrDraft] = useState<string | null>(null);
  const [ocrSaved, setOcrSaved] = useState("");
  const [ocrState, setOcrState] = useState<SaveState>("idle");

  // 三块内容与备注同构：textarea 常驻，按钮按焦点与脏值显隐。
  const [summaryFocused, setSummaryFocused] = useState(false);
  const [ocrFocused, setOcrFocused] = useState(false);
  const [bodyFocused, setBodyFocused] = useState(false);



  // 保存是异步的：期间切到别的素材时，旧的结果不能写进新素材的本地状态。
  const activeId = useRef(item.id);

  // 换素材时重置全部本地状态，避免上一条的备注/草稿串到下一条。
  useEffect(() => {
    activeId.current = item.id;
    setNote(null);
    setSavedNote("");
    setNoteFocused(false);
    setSaveState("idle");
    setTagDraft(null);
    setOverlayMode(null);
    setConfirmOpen(false);
    setReparseBusy(false);
    setReferenceBusy(false);
    setSummaryDraft(null);
    setSummarySaved("");
    setSummaryFocused(false);
    setSummaryState("idle");
    setBodyDraft(null);
    setBodySaved("");
    setBodyFocused(false);
    setBodyState("idle");
    setOcrDraft(null);
    setOcrSaved("");
    setOcrFocused(false);
    setOcrState("idle");
  }, [item.id]);

  // 真实备注到达后作为初始值；用户已经开始编辑则不覆盖。
  useEffect(() => {
    if (detail.data !== undefined && note === null) {
      setNote(detail.data.item.note);
      setSavedNote(detail.data.item.note);
    }
  }, [detail.data, note]);

  // 摘要 / OCR 与备注同构：尚未开始编辑（草稿为 null）时同时回填草稿与已保存值。
  useEffect(() => {
    if (detail.data === undefined) {
      return;
    }
    const loaded = detail.data.extras;
    if (summaryDraft === null) {
      const value = loaded.summary ?? "";
      setSummaryDraft(value);
      setSummarySaved(value);
    }
    if (ocrDraft === null) {
      const value = loaded.ocrText ?? "";
      setOcrDraft(value);
      setOcrSaved(value);
    }
    if (bodyDraft === null) {
      const value = loaded.bodyText ?? "";
      setBodyDraft(value);
      setBodySaved(value);
    }
  }, [detail.data, summaryDraft, bodyDraft, ocrDraft]);



  const onNoteChange = (value: string) => {
    // 上限阻止：超出部分直接不接受，而不是存进去再报错。
    const next = value.length > NOTE_LIMIT ? value.slice(0, NOTE_LIMIT) : value;
    setNote(next);
    setSaveState("idle");
  };

  /** 只有点「保存」才落盘：草稿与已保存内容不同才算有改动。 */
  const commitNote = async () => {
    if (note === null || note === savedNote) {
      return;
    }
    const id = item.id;
    const text = note;
    setSaveState("saving");
    try {
      await api.saveNote(id, text);
      if (activeId.current !== id) {
        return;
      }
      setSavedNote(text);
      setSaveState("saved");
    } catch (error) {
      // 失败不回滚输入：文本还在框里，用户可以直接再点保存。
      pushToast(errorMessage(error), "error");
      if (activeId.current !== id) {
        return;
      }
      setSaveState("error");
    }
  };

  /** 取消：丢弃草稿，恢复到上次保存的内容。 */
  const cancelNote = () => {
    setNote(savedNote);
    setSaveState("idle");
  };

  /** 三块内容的通用提交：只有真正改动才写库，失败保留草稿供重试。 */
  const commitContent = async (
    draft: string | null,
    saved: string,
    write: (id: string, text: string) => Promise<unknown>,
    applySaved: (text: string) => void,
    applyState: (state: SaveState) => void,
  ) => {
    if (draft === null || draft === saved) {
      return;
    }
    const id = item.id;
    const text = draft;
    applyState("saving");
    try {
      await write(id, text);
      if (activeId.current !== id) {
        return;
      }
      applySaved(text);
      applyState("saved");
    } catch (error) {
      // 与备注一致：失败不回滚输入，文本还在框里，可以直接再点保存。
      pushToast(errorMessage(error), "error");
      if (activeId.current !== id) {
        return;
      }
      applyState("error");
    }
  };

  const commitSummary = () =>
    commitContent(summaryDraft, summarySaved, api.setSummary, setSummarySaved, setSummaryState);

  const commitBody = () =>
    commitContent(bodyDraft, bodySaved, api.setBodyText, setBodySaved, setBodyState);

  const commitOcr = () =>
    commitContent(ocrDraft, ocrSaved, api.setOcrText, setOcrSaved, setOcrState);

  const copy = async () => {
    try {
      await api.copyOriginal(item.id, item.kind);
      pushToast(`已复制${copyLabel(item.kind).replace(/^复制/, "")}`);
    } catch (error) {
      // 按错误码给出可操作的提示，而不是把后端 message 原样抛给用户。
      const code = errorCode(error);
      if (code === "FILE_MISSING") {
        pushToast("原图文件缺失", "error");
      } else if (code === "CLIPBOARD_BUSY") {
        pushToast("剪贴板被占用，请重试", "error");
      } else {
        pushToast(errorMessage(error), "error");
      }
    }
  };

  const addTag = async (name: string) => {
    const trimmed = name.trim();
    setTagDraft(null);
    if (trimmed === "") {
      return;
    }
    try {
      await api.addTag(item.id, trimmed);
    } catch (error) {
      pushToast(errorMessage(error), "error");
    }
  };

  const removeItem = async () => {
    setConfirmOpen(false);
    try {
      await api.deleteItem(item.id);
      onClose();
    } catch (error) {
      if (errorCode(error) === "NOT_FOUND") {
        // 素材已被删掉：提示一句并收起面板，列表由事件刷新。
        pushToast("该素材已不存在", "error");
        onClose();
        return;
      }
      pushToast(errorMessage(error), "error");
    }
  };

  const reparse = async () => {
    setReparseBusy(true);
    try {
      await api.reparseItem(item.id);
      // `item_updated` 事件只刷列表不刷详情；这里手动失效一次让状态行立即更新，
      // 解析完成后 `item_enriched` 会再自动刷新摘要与标签。
      void queryClient.invalidateQueries({ queryKey: queryKeys.detail(item.id) });
      pushToast("已重新开始解析", "info");
    } catch (error) {
      if (errorCode(error) === "NOT_FOUND") {
        pushToast("该素材已不存在", "error");
        onClose();
        return;
      }
      pushToast(errorMessage(error), "error");
    } finally {
      setReparseBusy(false);
    }
  };

  /** 复制一段文本到剪贴板；失败给可操作提示。 */
  const copyText = async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text);
      pushToast(`已复制${label}`);
    } catch {
      pushToast("复制失败：剪贴板不可用", "error");
    }
  };

  const copySummary = async () => {
    if (summary === null) {
      return;
    }
    await copyText(summary, "摘要");
  };

  const removeTag = async (tagId: string, name: string) => {
    try {
      await api.removeTag(item.id, tagId);
    } catch (error) {
      pushToast(errorMessage(error), "error");
      return;
    }
    // 成功后靠 `item_tags_updated` 事件刷新，本地不删数组（避免与后端状态漂移）。
    pushToast(`已移除标签「${name}」`);
  };

  const extras = detail.data?.extras;
  const tags: TagChip[] = extras?.tags ?? [];
  const summary = extras?.summary ?? null;
  const summarySource = extras?.summarySource ?? "none";
  // 生成的 DTO 把 f32 映射成 `number | null`：这里统一成数字并丢掉零尺寸的脏框
  const ocrBoxes = (extras?.ocrBoxes ?? [])
    .map((box) => ({
      text: box.text,
      x: box.x ?? 0,
      y: box.y ?? 0,
      w: box.w ?? 0,
      h: box.h ?? 0,
    }))
    .filter((box) => box.w > 0 && box.h > 0);
  const bodyText = extras?.bodyText ?? null;
  const reference = extras?.reference ?? null;
  // `get_item_ext` 返回的是逻辑协议 URL，必须转成 WebView 能加载的地址，
  // 否则覆盖层加载失败后会静默回退到 480px 缩略图（看起来「不是原图」）。
  const originalUrl = toOriginalSrc(extras?.originalUrl ?? null);
  const full = detail.data?.item;
  const title = displayTitle(item);
  const renamed = (item.title?.trim() ?? "").length > 0;
  const kind = kindLabel(item.kind);
  const sizeText =
    full?.width != null && full.height != null ? `${full.width} × ${full.height}` : "—";

  const noteText = note ?? "";
  const noteDirty = note !== null && note !== savedNote;
  const noteSaving = saveState === "saving";
  // 只在「正在写」或「有未保存改动」时露出按钮：失焦后若仍有改动则保留，避免改完没处保存。
  const noteActionsVisible = noteFocused || noteDirty;
  const noteMeta = noteSaving
    ? "保存中…"
    : saveState === "error"
      ? "保存失败 · 文本未丢失"
      : saveState === "saved"
        ? "已保存"
        : noteDirty
          ? "未保存"
          : "";

  const summaryText = summaryDraft ?? "";
  const summaryDirty = summaryDraft !== null && summaryDraft !== summarySaved;
  const summaryActionsVisible = summaryFocused || summaryDirty;
  const bodyTruncated = extras?.bodyTextTruncated ?? false;
  const bodyEditable =
    item.kind === "text" && (extras?.bodyTextEditable ?? false) && !bodyTruncated;
  const bodyDirty = bodyDraft !== null && bodyDraft !== bodySaved;
  const bodyActionsVisible = bodyFocused || bodyDirty;
  const ocrValue = extras?.ocrText ?? "";
  const ocrTruncated = extras?.ocrTextTruncated ?? false;
  const ocrEditable = item.kind === "image" && ocrValue.length > 0 && !ocrTruncated;
  const ocrDirty = ocrDraft !== null && ocrDraft !== ocrSaved;
  const ocrActionsVisible = ocrFocused || ocrDirty;

  const openReference = async (reveal: boolean) => {
    setReferenceBusy(true);
    try {
      await api.openReferenceFile(item.id, reveal);
    } catch (error) {
      pushToast(errorMessage(error), "error");
    } finally {
      setReferenceBusy(false);
    }
  };

  return (
    <>
      <div className="detail-head">
        <span className="label">详情</span>
        <div className="detail-head-right">
          <span className="label">{item.id.slice(0, 8).toUpperCase()}</span>
          <button
            type="button"
            className="detail-close"
            title="关闭详情"
            aria-label="关闭详情"
            onClick={onClose}
          >
            <svg width="12" height="12" viewBox="0 0 14 14" stroke="currentColor" strokeWidth="1.4">
              <path d="M2.6 2.6l8.8 8.8M11.4 2.6l-8.8 8.8" />
            </svg>
          </button>
        </div>
      </div>

      <div className="detail-scroll">
        <div className="detail-inner" key={item.id}>
          {item.kind === "image" ? (
            <button
              type="button"
              className="d-thumb"
              onClick={() => setOverlayMode("plain")}
              aria-label="查看原图"
              title="查看原图"
            >
              {item.thumbUrl !== null ? (
                <img src={item.thumbUrl} alt={`${kind}素材缩略图`} draggable={false} />
              ) : (
                <span className="thumb-fallback">{kind}</span>
              )}
            </button>
          ) : (
            <div className="d-thumb">
              <span className="thumb-fallback">{kind}</span>
            </div>
          )}

          <div className="d-title-row">
            <h2 className="d-title">{title}</h2>
            {onRename !== undefined && (
              <button
                type="button"
                className="d-rename"
                title={`重命名 ${title}`}
                aria-label="重命名"
                onClick={() => onRename(item)}
              >
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none" aria-hidden="true">
                  <path
                    d="M10.4 2.6l3 3L5.6 13.4 2 14l.6-3.6 7.8-7.8z"
                    stroke="currentColor"
                    strokeWidth="1.4"
                    strokeLinejoin="round"
                  />
                </svg>
              </button>
            )}
          </div>
          {renamed && item.sourceTitle !== null && item.sourceTitle.trim().length > 0 && (
            <p className="d-source-title" title={item.sourceTitle}>
              来源：{item.sourceTitle}
            </p>
          )}
          <div className="d-src">
            <span className="mono">{item.sourceApp ?? "未知来源"}</span>
            <span className="dot" />
            <span className="mono">{formatRelativeTime(item.capturedAt)}</span>
            <span className="dot" />
            <span className="mono">{statusLabel(item.status)}</span>
          </div>

          <section className="d-sec">
            <div className="d-sec-head">
              <span className="label">AI 描述</span>
              {summary !== null && summarySource !== "none" && (
                <span className={`d-source-badge src-${summarySource}`} title="摘要来源">
                  {SUMMARY_SOURCE_LABEL[summarySource] ?? summarySource}
                </span>
              )}
              <div className="d-sec-actions">
                {summary !== null && (
                  <span className="mono d-sec-meta">
                    {Array.from(summaryText).length} / {SUMMARY_LIMIT} ·{" "}
                    {saveStateMeta(summaryState, summaryDirty)}
                  </span>
                )}
                <button
                  type="button"
                  className="mini-btn"
                  disabled={summary === null}
                  title={summary === null ? "还没有摘要" : "复制摘要"}
                  onClick={() => void copySummary()}
                >
                  复制
                </button>
              </div>
            </div>
            {summary === null ? (
              <p className="d-summary is-placeholder">{SUMMARY_PLACEHOLDER}</p>
            ) : (
              <div
                className={`d-note-wrap${summaryActionsVisible ? " has-actions" : ""}`}
                onFocus={() => setSummaryFocused(true)}
                onBlur={(event) => {
                  if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
                    setSummaryFocused(false);
                  }
                }}
              >
                <textarea
                  className="d-note d-content-edit"
                  value={summaryText}
                  spellCheck={false}
                  aria-label="编辑 AI 描述"
                  placeholder="写一段能帮你找回这条素材的描述…"
                  onChange={(event) => {
                    setSummaryDraft(clampToChars(event.target.value, SUMMARY_LIMIT));
                    setSummaryState("idle");
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Escape") {
                      event.stopPropagation();
                      setSummaryDraft(summarySaved);
                      setSummaryState("idle");
                      event.currentTarget.blur();
                    }
                  }}
                />
                {summaryActionsVisible && (
                  <div className="d-note-actions">
                    <button
                      type="button"
                      className="mini-btn"
                      aria-label="取消编辑 AI 描述"
                      disabled={!summaryDirty || summaryState === "saving"}
                      onClick={() => {
                        setSummaryDraft(summarySaved);
                        setSummaryState("idle");
                      }}
                    >
                      取消
                    </button>
                    <button
                      type="button"
                      className="mini-btn primary"
                      aria-label="保存 AI 描述"
                      disabled={!summaryDirty || summaryState === "saving"}
                      onClick={() => void commitSummary()}
                    >
                      保存
                    </button>
                  </div>
                )}
              </div>
            )}
            {ocrBoxes.length > 0 && (
              <button
                type="button"
                className="mini-btn d-highlight-btn"
                onClick={() => setOverlayMode("boxes")}
                title="在原图上标出识别到的文字"
              >
                在图中查看 {ocrBoxes.length} 处文字
              </button>
            )}
          </section>

          {item.kind === "text" && (
            <section className="d-sec">
              <div className="d-sec-head">
                <span className="label">原文</span>
                <div className="d-sec-actions">
                  {bodyEditable && (
                    <span className="mono d-sec-meta">
                      {Array.from(bodyDraft ?? "").length} / {CONTENT_LIMIT} ·{" "}
                      {saveStateMeta(bodyState, bodyDirty)}
                    </span>
                  )}
                  <button
                    type="button"
                    className="mini-btn"
                    aria-label="复制正文"
                    title="复制原文内容"
                    disabled={bodyText === null || bodyText.length === 0}
                    onClick={() => void copyText(bodyText ?? "", "原文")}
                  >
                    复制
                  </button>
                </div>
              </div>
              {bodyEditable ? (
                <div
                  className={`d-note-wrap${bodyActionsVisible ? " has-actions" : ""}`}
                  onFocus={() => setBodyFocused(true)}
                  onBlur={(event) => {
                    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
                      setBodyFocused(false);
                    }
                  }}
                >
                  <textarea
                    className="d-note d-content-edit"
                    value={bodyDraft ?? ""}
                    spellCheck={false}
                    aria-label="编辑原文"
                    onChange={(event) => {
                      setBodyDraft(clampToChars(event.target.value, CONTENT_LIMIT));
                      setBodyState("idle");
                    }}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") {
                        event.stopPropagation();
                        setBodyDraft(bodySaved);
                        setBodyState("idle");
                        event.currentTarget.blur();
                      }
                    }}
                  />
                  {bodyActionsVisible && (
                    <div className="d-note-actions">
                      <button
                        type="button"
                        className="mini-btn"
                        aria-label="取消编辑原文"
                        disabled={!bodyDirty || bodyState === "saving"}
                        onClick={() => {
                          setBodyDraft(bodySaved);
                          setBodyState("idle");
                        }}
                      >
                        取消
                      </button>
                      <button
                        type="button"
                        className="mini-btn primary"
                        aria-label="保存原文"
                        disabled={!bodyDirty || bodyState === "saving"}
                        onClick={() => void commitBody()}
                      >
                        保存
                      </button>
                    </div>
                  )}
                </div>
              ) : (
                <p className={bodyText === null ? "d-stub" : "d-summary"}>
                  {bodyText === null
                    ? "原文不可读（文件缺失或不是 UTF-8 文本）"
                    : bodyText.length === 0
                      ? "（正文为空）"
                      : bodyText}
                </p>
              )}
              {bodyEditable && bodyText === null && (
                <p className="d-edit-hint">
                  原文文件缺失或不是 UTF-8 文本，可直接在上面补录。
                </p>
              )}
              {!bodyEditable && !bodyTruncated && (
                <p className="d-edit-hint">该素材引用外部文件，原文只读。</p>
              )}
              {bodyTruncated && (
                <p className="d-edit-hint">
                  原文超过 {CONTENT_LIMIT} 字符，暂不支持在应用内编辑。
                </p>
              )}
            </section>
          )}

          {item.kind === "image" && (
            <section className="d-sec">
              <div className="d-sec-head">
                <span className="label">识别到的文字</span>
                <div className="d-sec-actions">
                  {ocrEditable && (
                    <span className="mono d-sec-meta">
                      {Array.from(ocrDraft ?? "").length} / {CONTENT_LIMIT} ·{" "}
                      {saveStateMeta(ocrState, ocrDirty)}
                    </span>
                  )}
                  <button
                    type="button"
                    className="mini-btn"
                    aria-label="复制识别文字"
                    title="复制识别到的文字"
                    disabled={ocrValue.length === 0}
                    onClick={() => void copyText(ocrValue, "识别文字")}
                  >
                    复制
                  </button>
                </div>
              </div>
              {ocrEditable ? (
                <div
                  className={`d-note-wrap${ocrActionsVisible ? " has-actions" : ""}`}
                  onFocus={() => setOcrFocused(true)}
                  onBlur={(event) => {
                    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
                      setOcrFocused(false);
                    }
                  }}
                >
                  <textarea
                    className="d-note d-content-edit"
                    value={ocrDraft ?? ""}
                    spellCheck={false}
                    aria-label="编辑识别文字"
                    onChange={(event) => {
                      setOcrDraft(clampToChars(event.target.value, CONTENT_LIMIT));
                      setOcrState("idle");
                    }}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") {
                        event.stopPropagation();
                        setOcrDraft(ocrSaved);
                        setOcrState("idle");
                        event.currentTarget.blur();
                      }
                    }}
                  />
                  {ocrActionsVisible && (
                    <div className="d-note-actions">
                      <button
                        type="button"
                        className="mini-btn"
                        aria-label="取消编辑识别文字"
                        disabled={!ocrDirty || ocrState === "saving"}
                        onClick={() => {
                          setOcrDraft(ocrSaved);
                          setOcrState("idle");
                        }}
                      >
                        取消
                      </button>
                      <button
                        type="button"
                        className="mini-btn primary"
                        aria-label="保存识别文字"
                        disabled={!ocrDirty || ocrState === "saving"}
                        onClick={() => void commitOcr()}
                      >
                        保存
                      </button>
                    </div>
                  )}
                </div>
              ) : (
                <p className={ocrValue.length === 0 ? "d-stub" : "d-summary d-ocr-text"}>
                  {ocrValue.length === 0 ? "未识别到文字" : ocrValue}
                </p>
              )}
              {ocrEditable && (
                <p className="d-edit-hint">
                  手工修改后，原图文字框仍按上次识别结果标注。
                </p>
              )}
              {ocrTruncated && (
                <p className="d-edit-hint">
                  识别文字超过 {CONTENT_LIMIT} 字符，暂不支持在应用内编辑。
                </p>
              )}
            </section>
          )}

          <section className="d-sec">
            <div className="d-sec-head">
              <span className="label">标签</span>
              <button
                type="button"
                className="mini-btn"
                onClick={() => setTagDraft((current) => (current === null ? "" : null))}
              >
                ＋ 添加
              </button>
            </div>
            {tagDraft !== null && (
              <input
                className="field tag-input"
                autoFocus
                value={tagDraft}
                aria-label="新标签名称"
                placeholder="标签名，回车确认"
                onChange={(event) => setTagDraft(event.target.value)}
                onBlur={() => void addTag(tagDraft)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void addTag(tagDraft);
                  }
                  if (event.key === "Escape") {
                    event.stopPropagation();
                    setTagDraft(null);
                  }
                }}
              />
            )}
            <div className="tag-row">
              {tags.length === 0 ? (
                <span className="tag-empty">暂无标签</span>
              ) : (
                tags.map((tag) => (
                  <span
                    key={tag.id}
                    className={`tag${tag.auto ? " auto" : ""}`}
                    style={
                      tag.hue
                        ? ({ "--hue": `var(--hue-${tag.hue})` } as CSSProperties)
                        : undefined
                    }
                  >
                    {tag.name}
                    <button
                      type="button"
                      className="tag-remove"
                      aria-label={`移除标签 ${tag.name}`}
                      title={`移除标签 ${tag.name}`}
                      onClick={() => void removeTag(tag.id, tag.name)}
                    >
                      ×
                    </button>
                  </span>
                ))
              )}
            </div>
          </section>

          <section className="d-sec">
            <div className="d-sec-head">
              <span className="label">备注</span>
              <span className="mono d-sec-meta">
                {noteText.length > 0 ? `${noteText.length} / ${NOTE_LIMIT} · ` : ""}
                {noteMeta}
              </span>
            </div>
            <div
              className={`d-note-wrap${noteActionsVisible ? " has-actions" : ""}`}
              onFocus={() => setNoteFocused(true)}
              onBlur={(event) => {
                // 焦点只是在输入框和按钮之间移动时不算离开，按钮不能半路消失。
                if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
                  setNoteFocused(false);
                }
              }}
            >
              <textarea
                className="d-note"
                value={noteText}
                placeholder="点击添加备注…"
                spellCheck={false}
                aria-label="备注"
                onChange={(event) => onNoteChange(event.target.value)}
                onKeyDown={(event) => {
                  // Esc 等同于「取消」：丢弃草稿回到已保存内容，不顺手关掉面板。
                  if (event.key === "Escape") {
                    event.stopPropagation();
                    cancelNote();
                    event.currentTarget.blur();
                  }
                }}
              />
              {noteActionsVisible && (
                <div className="d-note-actions">
                  <button
                    type="button"
                    className="mini-btn"
                    disabled={!noteDirty || noteSaving}
                    onClick={cancelNote}
                  >
                    取消
                  </button>
                  <button
                    type="button"
                    className="mini-btn primary"
                    disabled={!noteDirty || noteSaving}
                    onClick={() => void commitNote()}
                  >
                    保存
                  </button>
                </div>
              )}
            </div>
          </section>

          {reference !== null && (
            <section className="d-reference" aria-labelledby="reference-title">
              <div className="d-section-title" id="reference-title">引用文件</div>
              <dl>
                <div><dt>来源目录</dt><dd title={reference.sourcePath}>{reference.sourcePath}</dd></div>
                <div><dt>相对路径</dt><dd title={reference.relativePath}>{reference.relativePath}</dd></div>
                <div><dt>状态</dt><dd>{REFERENCE_STATUS_LABEL[reference.status] ?? reference.status}</dd></div>
                <div><dt>最后索引</dt><dd>{reference.lastIndexedAt == null ? "—" : formatAbsoluteTime(reference.lastIndexedAt)}</dd></div>
              </dl>
              <div className="d-reference-actions">
                <button type="button" className="mini-btn" disabled={referenceBusy} onClick={() => void openReference(false)}>打开原文件</button>
                <button type="button" className="mini-btn" disabled={referenceBusy} onClick={() => void openReference(true)}>打开所在目录</button>
              </div>
            </section>
          )}

          <div className="d-actions">
            <button type="button" className="btn primary" onClick={() => void copy()}>
              {copyLabel(item.kind)}
            </button>
            <button
              type="button"
              className="btn"
              disabled={reparseBusy}
              title="用当前选用模型重新运行 OCR、索引与 AI 摘要"
              onClick={() => void reparse()}
            >
              {reparseBusy ? "重新解析中…" : "重新解析"}
            </button>
            <button
              type="button"
              className="btn icon"
              aria-label="删除素材"
              title="删除素材"
              onClick={() => setConfirmOpen(true)}
            >
              <svg
                width="13"
                height="13"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M4.5 7h15M9.5 7V5.4c0-.5.4-.9.9-.9h3.2c.5 0 .9.4.9.9V7" />
                <path d="M6.4 7l.8 11.4c0 .9.8 1.6 1.7 1.6h6.2c.9 0 1.7-.7 1.7-1.6L17.6 7" />
              </svg>
            </button>
          </div>

          <div className="d-meta-list">
            <div className="d-meta-row">
              <span>素材编号</span>
              <span>{item.id.toUpperCase()}</span>
            </div>
            <div className="d-meta-row">
              <span>尺寸</span>
              <span>{sizeText}</span>
            </div>
            <div className="d-meta-row">
              <span>来源</span>
              <span>{item.sourceApp ?? "—"}</span>
            </div>
            <div className="d-meta-row">
              <span>采集时间</span>
              <span>{formatAbsoluteTime(item.capturedAt)}</span>
            </div>
          </div>
        </div>
      </div>

      {overlayMode !== null && (
        <ImageOverlay
          originalUrl={originalUrl}
          thumbUrl={item.thumbUrl}
          title={title}
          kind={item.kind}
          // 只有「在图中查看 N 处文字」这个入口才叠加 OCR 高亮框。
          boxes={overlayMode === "boxes" ? ocrBoxes : []}
          onClose={() => setOverlayMode(null)}
        />
      )}

      {confirmOpen && (
        <ConfirmDialog
          title="彻底删除这条素材？"
          description={reference !== null ? "索引、备注、标签与派生缓存会一并移除；外部原文件不会被修改或删除，之后重新扫描该目录仍可再次导入。" : "原图、缩略图与索引条目会一并移除，不做软删除，不可恢复。"}
          confirmLabel="彻底删除"
          danger
          onConfirm={() => void removeItem()}
          onCancel={() => setConfirmOpen(false)}
        />
      )}
    </>
  );
}

/** 未选中素材时的详情列空态（原型 `.detail-empty`）。 */
export function DetailEmpty() {
  return (
    <>
      <div className="detail-head">
        <span className="label">详情</span>
        <span className="label">—</span>
      </div>
      <div className="detail-empty">
        <div className="ring">
          <svg
            width="15"
            height="15"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.4"
          >
            <path d="M5 12h14" strokeLinecap="round" />
          </svg>
        </div>
        <span className="detail-empty-text">从中间选一条素材</span>
      </div>
    </>
  );
}
