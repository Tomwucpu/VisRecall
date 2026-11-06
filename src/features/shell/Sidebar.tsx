import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { queryKeys } from "../../lib/queryClient";

type Props = {
  collapsed: boolean;
  onToggle: () => void;
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
 * 标签云与存储条走 `api` stub：标签为空数组、用量为 null，界面显示「暂无标签」与
 * 「—」而不是编造数字。折叠是纯前端状态（B 键），不落盘。
 */
export function Sidebar({ collapsed, onToggle, itemCount }: Props) {
  const tags = useQuery({ queryKey: queryKeys.tags, queryFn: () => api.listTags() });
  const tagList = tags.data ?? [];

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
              {tagList.map((tag) => (
                <span key={tag.id} className="tag-pill" title="标签筛选待阶段 03 提供">
                  <span className="hash">#</span>
                  {tag.name}
                </span>
              ))}
            </div>
          )}
        </div>
      </div>

      <div className="side-foot" title="存储用量待阶段 02 提供">
        <div className="storage-line">
          <span>已用 —</span>
          <span>—</span>
        </div>
        <div className="storage-bar">
          <i />
        </div>
      </div>
    </aside>
  );
}
