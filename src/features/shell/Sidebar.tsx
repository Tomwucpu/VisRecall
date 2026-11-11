import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type TagChip } from "../../lib/api";
import { errorMessage } from "../../lib/errors";
import { formatBytes } from "../../lib/format";
import { queryKeys } from "../../lib/queryClient";
import { pushToast } from "../../lib/toast";
import { ConfirmDialog } from "../common/ConfirmDialog";

type Props = {
  collapsed: boolean;
  onToggle: () => void;
  /** 点标签 → 切换该标签的筛选（再点一次取消）。 */
  onTagFilter?: (name: string) => void;
  /** 当前生效的标签筛选（可多选，用于高亮与取消）。 */
  activeTags?: string[];
  /** 已加载素材数；`null` 表示列表还没就绪。 */
  itemCount: number | null;
};

/**
 * 侧栏（T6）。
 *
 * 只放 V0.1 范围内的内容：视图组里的「全部素材」与常用标签云。收件箱/星标/合集
 * 属 V0.5（F4.1–F4.2、F4.5–F4.7），按规划 §7 不进 DOM —— 所以这里没有可点的死项：
 * 「全部素材」在当前视图下是静态的当前位置（`aria-current`），不是按钮。
 *
 * 标签云与存储条都读真实数据：标签来自 `tags_list`（含每标签素材数），用量来自
 * `storage_usage_get`。读不到时显示「—」而不是 0 —— 「读不到」与「真的是 0」必须区分。
 * 折叠是纯前端状态（B 键），不落盘。
 */
export function Sidebar({ collapsed, onToggle, itemCount, onTagFilter, activeTags = [] }: Props) {
  const tags = useQuery({ queryKey: queryKeys.tags, queryFn: () => api.listTags() });
  const usage = useQuery({
    queryKey: queryKeys.usage,
    queryFn: () => api.getStorageUsage(),
    // 用量统计要遍历目录，别每次切换都重算（P2-R07 的 150ms 预算留给真正需要时）。
    staleTime: 30_000,
  });

  const [menuTagId, setMenuTagId] = useState<string | null>(null);
  const [renameTagId, setRenameTagId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [deleteTarget, setDeleteTarget] = useState<TagChip | null>(null);

  const tagList = tags.data ?? [];

  const submitRename = async (tagId: string, name: string) => {
    setRenameTagId(null);
    const trimmed = name.trim();
    const original = tagList.find((tag) => tag.id === tagId);
    if (trimmed === "" || trimmed === original?.name) {
      return;
    }
    try {
      await api.renameTag(tagId, trimmed);
    } catch (error) {
      pushToast(errorMessage(error), "error");
    }
  };

  const confirmDelete = async (tag: TagChip) => {
    setDeleteTarget(null);
    try {
      const affected = await api.deleteTag(tag.id);
      pushToast(`已删除标签「${tag.name}」，解除 ${affected} 条关联`);
    } catch (error) {
      pushToast(errorMessage(error), "error");
    }
  };

  const usedText =
    usage.data === undefined
      ? "已用 —"
      : `已用 ${formatBytes(usage.data.usedBytes)} / ${formatBytes(usage.data.capacityBytes)}`;
  const ratio =
    usage.data === undefined || usage.data.capacityBytes <= 0
      ? 0
      : Math.min(usage.data.usedBytes / usage.data.capacityBytes, 1);

  return (
    <aside className={`sidebar${collapsed ? " collapsed" : ""}`}>
      <div className="sidebar-scroll">
        <div className="side-top">
          <span className="label">素材库</span>
          <button
            type="button"
            className="side-toggle"
            onClick={onToggle}
            title={collapsed ? "展开侧栏  B" : "折叠侧栏  B"}
            aria-label={collapsed ? "展开侧栏" : "折叠侧栏"}
            aria-expanded={!collapsed}
          >
            <svg
              className="ico-fold"
              width="15"
              height="15"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <rect x="3.5" y="4.5" width="17" height="15" rx="2.5" />
              <path d="M9.6 4.8v14.4M16 9.5L13.5 12l2.5 2.5" />
            </svg>
            <svg
              className="ico-unfold"
              width="15"
              height="15"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <rect x="3.5" y="4.5" width="17" height="15" rx="2.5" />
              <path d="M9.6 4.8v14.4M13.4 9.5L15.9 12l-2.5 2.5" />
            </svg>
          </button>
        </div>

        <div className="side-group">
          <div className="side-item active" aria-current="page" title="全部素材">
            <span className="ico">
              <svg
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinejoin="round"
              >
                <path d="M12 3.6l8.4 4.6L12 12.8 3.6 8.2 12 3.6z" />
                <path d="M3.6 13L12 17.6 20.4 13" strokeLinecap="round" />
              </svg>
            </span>
            <span>全部素材</span>
            <span className="count">{itemCount ?? "—"}</span>
          </div>
        </div>

        <div className="side-group">
          <div className="side-group-head">
            <span className="label">常用标签</span>
          </div>
          {tagList.length === 0 ? (
            <div className="tag-cloud-empty">暂无标签</div>
          ) : (
            <div className="tag-cloud">
              {tagList.map((tag) =>
                renameTagId === tag.id ? (
                  <input
                    key={tag.id}
                    className="field tag-input"
                    autoFocus
                    value={renameDraft}
                    aria-label={`重命名标签 ${tag.name}`}
                    onChange={(event) => setRenameDraft(event.target.value)}
                    onBlur={() => void submitRename(tag.id, renameDraft)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") {
                        event.preventDefault();
                        void submitRename(tag.id, renameDraft);
                      }
                      if (event.key === "Escape") {
                        event.stopPropagation();
                        setRenameTagId(null);
                      }
                    }}
                  />
                ) : (
                  <span
                    key={tag.id}
                    className={`tag-pill${activeTags.includes(tag.name) ? " is-active" : ""}`}
                    title={
                      activeTags.includes(tag.name)
                        ? `取消「${tag.name}」筛选`
                        : `按标签「${tag.name}」筛选`
                    }
                    role={onTagFilter !== undefined ? "button" : undefined}
                    tabIndex={onTagFilter !== undefined ? 0 : undefined}
                    aria-pressed={
                      onTagFilter !== undefined ? activeTags.includes(tag.name) : undefined
                    }
                    onClick={
                      onTagFilter !== undefined ? () => onTagFilter(tag.name) : undefined
                    }
                    onKeyDown={
                      onTagFilter !== undefined
                        ? (event) => {
                            if (event.key === "Enter" || event.key === " ") {
                              event.preventDefault();
                              onTagFilter(tag.name);
                            }
                          }
                        : undefined
                    }
                  >
                    <span className="hash">#</span>
                    {tag.name}
                    <span className="tag-count">{tag.itemCount}</span>
                    <button
                      type="button"
                      className="tag-more"
                      aria-label={`标签操作 ${tag.name}`}
                      title={`标签操作 ${tag.name}`}
                      onClick={() =>
                        setMenuTagId((current) => (current === tag.id ? null : tag.id))
                      }
                    >
                      …
                    </button>
                    {menuTagId === tag.id && (
                      <span className="tag-menu" role="menu">
                        <button
                          type="button"
                          role="menuitem"
                          onClick={() => {
                            setMenuTagId(null);
                            setRenameTagId(tag.id);
                            setRenameDraft(tag.name);
                          }}
                        >
                          重命名
                        </button>
                        <button
                          type="button"
                          role="menuitem"
                          onClick={() => {
                            setMenuTagId(null);
                            setDeleteTarget(tag);
                          }}
                        >
                          删除
                        </button>
                      </span>
                    )}
                  </span>
                ),
              )}
            </div>
          )}
        </div>
      </div>

      <div className="side-foot" title="用量含 originals/、texts/、thumbs/ 与数据库文件">
        <div className="storage-line">
          <span>{usedText}</span>
          <span>{usage.data === undefined ? "—" : `${usage.data.itemCount} 条`}</span>
        </div>
        <div
          className="storage-bar"
          role="progressbar"
          aria-label="存储用量"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(ratio * 100)}
        >
          <i style={{ width: `${ratio * 100}%` }} />
        </div>
      </div>

      {deleteTarget !== null && (
        <ConfirmDialog
          title={`删除标签「${deleteTarget.name}」？`}
          description={`将影响 ${deleteTarget.itemCount} 条素材；标签本身与它的全部关联都会被移除，素材不会删除。`}
          confirmLabel="删除标签"
          danger
          onConfirm={() => void confirmDelete(deleteTarget)}
          onCancel={() => setDeleteTarget(null)}
        />
      )}
    </aside>
  );
}
