import { useCallback, useMemo, useRef, useState } from "react";

/**
 * 多选状态：模式开关 + 选择集 + Shift 区间锚点。
 *
 * 设计取舍：
 * - 只在「选择」模式下生效；退出模式一定清空选择，避免下一次误删上一次的残留；
 * - Shift 连选依赖当前**已加载顺序**（`orderedIds`），跨页区间不会命中未加载项；
 * - 「全选当前筛选」可能包含未加载 id，因此不在列表变化时自动裁剪——
 *   条件变化时由 `App` 显式清空（`clear`），语义是「换筛选 = 重新开始选择」。
 */
export function useSelection(orderedIds: string[]) {
  const [selectionMode, setSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  const anchorRef = useRef<string | null>(null);

  const enter = useCallback(() => {
    setSelectionMode(true);
    setSelectedIds(new Set());
    anchorRef.current = null;
  }, []);

  const exit = useCallback(() => {
    setSelectionMode(false);
    setSelectedIds(new Set());
    anchorRef.current = null;
  }, []);

  const clear = useCallback(() => {
    setSelectedIds(new Set());
    anchorRef.current = null;
  }, []);

  const toggle = useCallback(
    (id: string, shift: boolean = false) => {
      setSelectedIds((current) => {
        const next = new Set(current);

        if (shift && anchorRef.current !== null) {
          const from = orderedIds.indexOf(anchorRef.current);
          const to = orderedIds.indexOf(id);
          if (from !== -1 && to !== -1) {
            const [start, end] = from <= to ? [from, to] : [to, from];
            for (let index = start; index <= end; index += 1) {
              next.add(orderedIds[index]);
            }
            anchorRef.current = id;
            return next;
          }
        }

        if (next.has(id)) {
          next.delete(id);
        } else {
          next.add(id);
        }
        anchorRef.current = id;
        return next;
      });
    },
    [orderedIds],
  );

  /** 合并一批 id（侧栏「全选当前筛选」用）；调用方负责只在选择模式下调用。 */
  const selectMany = useCallback((ids: Iterable<string>) => {
    setSelectedIds((current) => {
      const next = new Set(current);
      for (const id of ids) {
        next.add(id);
      }
      return next;
    });
  }, []);

  const selectAllLoaded = useCallback(() => {
    setSelectedIds(new Set(orderedIds));
    anchorRef.current = orderedIds[0] ?? null;
  }, [orderedIds]);

  /** 删除成功后把已删 id 从选择集中摘掉（不必等列表刷新）。 */
  const remove = useCallback((ids: Iterable<string>) => {
    setSelectedIds((current) => {
      const next = new Set(current);
      for (const id of ids) {
        next.delete(id);
      }
      return next;
    });
    anchorRef.current = null;
  }, []);

  const count = selectedIds.size;
  const ids = useMemo(() => [...selectedIds], [selectedIds]);

  return {
    selectionMode,
    selectedIds,
    ids,
    count,
    enter,
    exit,
    clear,
    toggle,
    selectMany,
    selectAllLoaded,
    remove,
  };
}

export type SelectionController = ReturnType<typeof useSelection>;
