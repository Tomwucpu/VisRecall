/**
 * 视觉主色 → 主题色令牌（`--hue-*`）。
 *
 * 调色板只有 6 个色族（ash/indigo/jade/ochre/paper/plum/slate），而视觉主色有 12 种；
 * 因此是**就近归类**而不是一一对应。归类只影响卡片色点，不影响检索口径
 * （检索用的永远是 `insights.primary_color` 的枚举值）。
 */
const HUE_BY_COLOR: Record<string, string> = {
  red: "plum",
  orange: "ochre",
  yellow: "ochre",
  brown: "ochre",
  green: "jade",
  blue: "slate",
  purple: "plum",
  pink: "plum",
  black: "indigo",
  white: "paper",
  gray: "ash",
  multicolor: "indigo",
};

/** 未知/未解析一律中性灰（`--hue-ash`）。 */
export function hueForColor(primaryColor: string | null | undefined): string {
  if (primaryColor === null || primaryColor === undefined) {
    return "var(--hue-ash)";
  }
  const token = HUE_BY_COLOR[primaryColor.trim().toLowerCase()];
  return token === undefined ? "var(--hue-ash)" : `var(--hue-${token})`;
}

/** 颜色枚举的中文名（解释条与提示用）。 */
const COLOR_LABEL: Record<string, string> = {
  red: "红色",
  orange: "橙色",
  yellow: "黄色",
  green: "绿色",
  blue: "蓝色",
  purple: "紫色",
  pink: "粉色",
  brown: "棕色",
  black: "黑色",
  white: "白色",
  gray: "灰色",
  multicolor: "多彩",
};

export function colorLabel(color: string): string {
  return COLOR_LABEL[color.trim().toLowerCase()] ?? color;
}
