import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, SUMMARY_PLACEHOLDER, type TagChip } from "../../lib/api";
import { errorCode, errorMessage } from "../../lib/errors";
import { formatAbsoluteTime, formatRelativeTime, kindLabel, statusLabel } from "../../lib/format";
import { queryKeys } from "../../lib/queryClient";
import { pushToast } from "../../lib/toast";
import { ConfirmDialog } from "../common/ConfirmDialog";
import type { ItemCardData } from "../library/ItemCard";
import { ImageOverlay } from "./ImageOverlay";

type Props = {
  item: ItemCardData;
  onClose: () => void;
};/** 备注上限（契约草案：10000 字，超限阻止输入而不是截断保存）。 */
const NOTE_LIMIT = 10_000;
const NOTE_DEBOUNCE_MS = 300;

type SaveState = "idle" | "saving" | "saved" | "error";

function copyLabelOf(kind: string): string {
  if (kind === "image") return "复制原图";
  if (kind === "link") return "复制链接";
  return "复制原文";
}

/**
 * 详情面板（T4）。
 *
 * 数据分层：基础字段来自真实 `get_item`，AI 描述 / 原文 / 原图 / 标签走 `api` stub，
 * 缺能力处一律显式占位（「待阶段 02 提供」），不显示任何伪造内容。
 * AI 描述区块只渲染 `SUMMARY_PLACEHOLDER`，复制按钮始终禁用（红线）。
 */
export function DetailPanel({ item, onClose }: Props) {
  const detail = useQuery({
    queryKey: queryKeys.detail(item.id),
    queryFn: () => api.loadDetail(item.id),
  });

  const [note, setNote] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [tagDraft, setTagDraft] = useState<string | null>(null);
  const [overlayOpen, setOverlayOpen] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pending = useRef<string | null>(null);
  const savedText = useRef<string>("");

  // 换素材时重置全部本地状态，避免上一条的备注/草稿串到下一条。
  useEffect(() => {
    setNote(null);
    setSaveState("idle");
    setTagDraft(null);
    setOverlayOpen(false);
    setConfirmOpen(false);
    pending.current = null;
    savedText.current = "";
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
  }, [item.id]);

  // 真实备注到达后作为初始值；用户已经开始编辑则不覆盖。
  useEffect(() => {
    if (detail.data !== undefined && note === null) {
      setNote(detail.data.item.note);
      savedText.current = detail.data.item.note;
    }
  }, [detail.data, note]);

  const flushNote = useCallback(
    async (text: string) => {
      if (text === savedText.current) {
        return;
      }
      setSaveState("saving");
      try {
        await api.saveNote(item.id, text);
        savedText.current = text;
        setSaveState("saved");
      } catch (error) {
        // 失败时不回滚用户输入：文本还在输入框里，用户可以直接重试。
        setSaveState("error");
        pushToast(errorMessage(error), "error");
      }
    },
    [item.id],
  );

  const scheduleSave = useCallback(
    (text: string) => {
      pending.current = text;
      if (timer.current !== null) {
        clearTimeout(timer.current);
      }
      timer.current = setTimeout(() => {
        timer.current = null;
        const value = pending.current;
        pending.current = null;
        if (value !== null) {
          void flushNote(value);
        }
      }, NOTE_DEBOUNCE_MS);
    },
    [flushNote],
  );

  // 卸载前把未落盘的草稿冲一次，避免「打了字就切走」丢内容。
  useEffect(() => {
    return () => {
      if (timer.current !== null) {
        clearTimeout(timer.current);
        timer.current = null;
      }
      if (pending.current !== null) {
        const value = pending.current;
        pending.current = null;
        void flushNote(value);
      }
    };
  }, [flushNote]);

  const onNoteChange = (value: string) => {
    // 上限阻止：超出部分直接不接受，而不是存进去再报错。
    const next = value.length > NOTE_LIMIT ? value.slice(0, NOTE_LIMIT) : value;
    setNote(next);
    setSaveState("idle");
    scheduleSave(next);
  };

  const flushNow = () => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    const value = pending.current;
    pending.current = null;
    if (value !== null) {
      void flushNote(value);
    }
  };

  const copy = async () => {
    try {
      await api.copyOriginal(item.id, item.kind);
      pushToast(`已复制${copyLabelOf(item.kind).replace(/^复制/, "")}`);
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
  const bodyText = extras?.bodyText ?? null;
  const originalUrl = extras?.originalUrl ?? null;
  const full = detail.data?.item;
  const title = item.sourceTitle ?? "未命名来源";
  const kind = kindLabel(item.kind);
  const sizeText =
    full?.width != null && full.height != null ? `${full.width} × ${full.height}` : "—";

  const noteText = note ?? "";
  const noteMeta =
    saveState === "saving"
      ? "保存中…"
      : saveState === "saved"
        ? "已保存"
        : saveState === "error"
          ? "保存失败 · 文本未丢失"
          : "行内编辑 · 自动保存";

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
              onClick={() => setOverlayOpen(true)}
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

          <h2 className="d-title">{title}</h2>
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
              {/* 阶段 03 才有摘要，复制在数据到来前保持禁用（红线）。 */}
              <button type="button" className="mini-btn" disabled title="阶段 03 提供">
                复制
              </button>
            </div>
            <p className={`d-summary${summary === null ? " is-placeholder" : ""}`}>
              {summary ?? SUMMARY_PLACEHOLDER}
            </p>
          </section>

          {item.kind === "text" && (
            <section className="d-sec">
              <div className="d-sec-head">
                <span className="label">原文</span>
              </div>
              <p className={bodyText === null ? "d-stub" : "d-summary"}>
                {bodyText ?? "原文不可读（文件缺失或不是 UTF-8 文本）"}
              </p>
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
            <textarea
              className="d-note"
              value={noteText}
              placeholder="点击添加备注…"
              spellCheck={false}
              aria-label="备注"
              onChange={(event) => onNoteChange(event.target.value)}
              onBlur={flushNow}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.stopPropagation();
                  flushNow();
                  event.currentTarget.blur();
                }
              }}
            />
          </section>

          <div className="d-actions">
            <button type="button" className="btn primary" onClick={() => void copy()}>
              {copyLabelOf(item.kind)}
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

      {overlayOpen && (
        <ImageOverlay
          originalUrl={originalUrl}
          thumbUrl={item.thumbUrl}
          title={title}
          kind={item.kind}
          onClose={() => setOverlayOpen(false)}
        />
      )}

      {confirmOpen && (
        <ConfirmDialog
          title="彻底删除这条素材？"
          description="原图、缩略图与索引条目会一并移除，不做软删除，不可恢复。"
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
