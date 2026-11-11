import { useEffect, useRef, useState, type ReactNode } from "react";
import { kindLabel, statusLabel } from "../../lib/format";
import {
  KIND_OPTIONS,
  STATUS_OPTIONS,
  TIME_PRESET_OPTIONS,
  type FilterPillKey,
} from "../../lib/filters";
import type { ItemFiltersController } from "./useItemFilters";

type Props = {
  controller: ItemFiltersController;
  /** 侧栏标签云（没有标签时面板显示占位）。 */
  tagOptions: string[];
};

/** 一个筛选 pill + 浮层面板的通用外壳。 */
function Pill({
  label,
  count,
  open,
  onToggle,
  buttonRef,
  children,
}: {
  label: string;
  count: number;
  open: boolean;
  onToggle: () => void;
  buttonRef: (node: HTMLButtonElement | null) => void;
  children: ReactNode;
}) {
  return (
    <div className="filter-pill-wrap">
      <button
        ref={buttonRef}
        type="button"
        className={`filter-pill${count > 0 ? " is-active" : ""}`}
        aria-expanded={open}
        aria-haspopup="dialog"
        onClick={onToggle}
      >
        <span>{label}</span>
        {count > 0 && <span className="filter-pill-count">{count}</span>}
        <svg width="8" height="8" viewBox="0 0 10 10" aria-hidden="true">
          <path d="M1.6 3.2L5 6.6l3.4-3.4" fill="none" stroke="currentColor" strokeWidth="1.3" />
        </svg>
      </button>
      {open && (
        <div className="filter-panel" role="dialog" aria-label={`${label}筛选`}>
          {children}
        </div>
      )}
    </div>
  );
}

/** 复选行；用原生 checkbox，键盘与读屏都不用额外适配。 */
function CheckRow({
  checked,
  label,
  onChange,
}: {
  checked: boolean;
  label: string;
  onChange: () => void;
}) {
  return (
    <label className="filter-option">
      <input type="checkbox" checked={checked} onChange={onChange} />
      <span>{label}</span>
    </label>
  );
}

/**
 * 筛选条：类型 / 时间 / 标签 / 状态四个 pill，各自展开浮层面板。
 *
 * 面板内是「立即生效」的多选：勾一个就刷新结果，不设「应用」按钮——用户
 * 期待的是筛子实时收紧，而不是表单提交。点面板外、按 Esc 或再点 pill 关闭。
 */
export function FilterBar({ controller, tagOptions }: Props) {
  const [open, setOpen] = useState<FilterPillKey | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const buttons = useRef<Partial<Record<FilterPillKey, HTMLButtonElement | null>>>({});

  useEffect(() => {
    if (open === null) {
      return;
    }
    const onPointerDown = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) {
        setOpen(null);
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [open]);

  const toggle = (key: FilterPillKey) => {
    setOpen((current) => {
      if (current === key) {
        buttons.current[key]?.focus();
        return null;
      }
      return key;
    });
  };

  const close = () => {
    const key = open;
    setOpen(null);
    if (key !== null) {
      buttons.current[key]?.focus();
    }
  };

  const { state, counts } = controller;

  return (
    <div
      className="filter-bar"
      ref={rootRef}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          close();
        }
      }}
    >
      <span className="filter-bar-label">筛选</span>

      <Pill
        label="类型"
        count={counts.kinds}
        open={open === "kinds"}
        onToggle={() => toggle("kinds")}
        buttonRef={(node) => {
          buttons.current.kinds = node;
        }}
      >
        {KIND_OPTIONS.map((kind) => (
          <CheckRow
            key={kind}
            checked={state.kinds.includes(kind)}
            label={kindLabel(kind)}
            onChange={() => controller.toggleKind(kind)}
          />
        ))}
        <div className="filter-panel-foot">
          <button type="button" onClick={() => KIND_OPTIONS.forEach((kind) => state.kinds.includes(kind) && controller.toggleKind(kind))}>
            清空
          </button>
        </div>
      </Pill>

      <Pill
        label="时间"
        count={counts.time}
        open={open === "time"}
        onToggle={() => toggle("time")}
        buttonRef={(node) => {
          buttons.current.time = node;
        }}
      >
        <div className="filter-preset-grid">
          {TIME_PRESET_OPTIONS.map((option) => (
            <button
              key={option.key}
              type="button"
              className={`filter-preset${state.preset === option.key ? " is-active" : ""}`}
              onClick={() => {
                controller.choosePreset(option.key);
                close();
              }}
            >
              {option.label}
            </button>
          ))}
        </div>
        <div className="filter-custom">
          <span className="filter-sub">自定义</span>
          <div className="filter-date-row">
            <input
              type="date"
              aria-label="开始日期"
              value={state.customFrom}
              onChange={(event) =>
                controller.setCustomDates(event.target.value, state.customTo)
              }
            />
            <span className="filter-dash">—</span>
            <input
              type="date"
              aria-label="结束日期"
              value={state.customTo}
              onChange={(event) =>
                controller.setCustomDates(state.customFrom, event.target.value)
              }
            />
          </div>
        </div>
        <div className="filter-panel-foot">
          <button
            type="button"
            onClick={() => {
              controller.choosePreset(null);
              close();
            }}
          >
            不限时间
          </button>
        </div>
      </Pill>

      <Pill
        label="标签"
        count={counts.tags}
        open={open === "tags"}
        onToggle={() => toggle("tags")}
        buttonRef={(node) => {
          buttons.current.tags = node;
        }}
      >
        {tagOptions.length === 0 ? (
          <p className="filter-empty">还没有标签</p>
        ) : (
          <div className="filter-tag-list">
            {tagOptions.map((tag) => (
              <CheckRow
                key={tag}
                checked={state.tags.includes(tag)}
                label={`#${tag}`}
                onChange={() => controller.toggleTag(tag)}
              />
            ))}
          </div>
        )}
        <div className="filter-panel-foot">
          <button
            type="button"
            onClick={() => state.tags.forEach((tag) => controller.toggleTag(tag))}
          >
            清空
          </button>
        </div>
      </Pill>

      <Pill
        label="状态"
        count={counts.statuses}
        open={open === "statuses"}
        onToggle={() => toggle("statuses")}
        buttonRef={(node) => {
          buttons.current.statuses = node;
        }}
      >
        {STATUS_OPTIONS.map((status) => (
          <CheckRow
            key={status}
            checked={state.statuses.includes(status)}
            label={statusLabel(status)}
            onChange={() => controller.toggleStatus(status)}
          />
        ))}
        <div className="filter-panel-foot">
          <button
            type="button"
            onClick={() =>
              STATUS_OPTIONS.forEach(
                (status) => state.statuses.includes(status) && controller.toggleStatus(status),
              )
            }
          >
            清空
          </button>
        </div>
      </Pill>
    </div>
  );
}
