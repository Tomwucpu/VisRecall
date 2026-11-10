import { convertFileSrc } from "@tauri-apps/api/core";

/** 自定义协议名，与 `src-tauri/src/protocol.rs` 的 `SCHEME` 一致。 */
const ASSET_SCHEME = "visrecall";
type AssetKind = "thumb" | "original";

/**
 * `visrecall://<kind>/<id>` → 自定义协议的可加载地址（Windows 上是
 * `http://visrecall.localhost/<kind>/<id>`）。
 *
 * `convertFileSrc` 会把整段路径当成文件路径做 `encodeURIComponent`，冒号/斜杠会被
 * 编码成 `%3A`/`%2F`，协议处理器就只看到一段路径了。所以这里只交给它固定的
 * 资源类别段，再由自己拼 `/<id>`。非本协议的 URL（含已转换过的地址）原样返回，
 * 因此转换是幂等的。
 *
 * 后端会在 id 后面追加缓存失效参数（列表 `?rev=N`、原图 `?revision=N`）。查询串
 * 必须留在 `?` 之后原样拼接：一旦跟着 id 一起被编码成 `%3Frev%3D0`，协议处理器
 * 拿到的 id 就带百分号，会按非法 id 直接返回 400（图片因此加载失败）。
 */
function toAssetSrc(url: string | null, kind: AssetKind): string | null {
  if (!url) {
    return null;
  }
  const prefix = `${ASSET_SCHEME}://${kind}/`;
  if (!url.startsWith(prefix)) {
    return url;
  }
  const rest = url.slice(prefix.length);
  const suffixAt = rest.search(/[?#]/);
  const id = suffixAt === -1 ? rest : rest.slice(0, suffixAt);
  const query = suffixAt === -1 ? "" : rest.slice(suffixAt);
  return `${convertFileSrc(kind, ASSET_SCHEME)}/${encodeURIComponent(id)}${query}`;
}

/** `ItemDto.thumbUrl`（`visrecall://thumb/<id>`）→ WebView 可加载的缩略图地址。 */
export function toThumbSrc(url: string | null): string | null {
  return toAssetSrc(url, "thumb");
}

/** `DetailExtrasDto.originalUrl`（`visrecall://original/<id>`）→ WebView 可加载的原图地址。 */
export function toOriginalSrc(url: string | null): string | null {
  return toAssetSrc(url, "original");
}

const KNOWN_STATUSES = ["parsing", "ready", "pending", "failed"] as const;

/** 状态徽标文案（design §11.1：parsing/ready/pending/failed）；未知值给中文兜底。 */
export function statusLabel(status: string): string {
  switch (status) {
    case "parsing":
      return "解析中";
    case "ready":
      return "正常";
    case "pending":
      return "待解析";
    case "failed":
      return "失败";
    default:
      return "未知状态";
  }
}

/** 徽标样式后缀：未知状态统一落到默认样式，避免出现无样式的裸徽标。 */
export function statusTone(status: string): string {
  return (KNOWN_STATUSES as readonly string[]).includes(status) ? status : "unknown";
}

/** 主复制按钮文案：按素材类型显示「复制原图 / 复制原文 / 复制链接」。 */
export function copyLabel(kind: string): string {
  switch (kind) {
    case "image":
      return "复制原图";
    case "link":
      return "复制链接";
    default:
      return "复制原文";
  }
}

/** 素材类型文案；未知值给中文兜底。 */
export function kindLabel(kind: string): string {
  switch (kind) {
    case "image":
      return "图片";
    case "text":
      return "文字";
    case "link":
      return "链接";
    default:
      return "未知类型";
  }
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

function pad(value: number): string {
  return value.toString().padStart(2, "0");
}

/** 本地时间 `YYYY-MM-DD HH:mm`；用于 `title` 等需要精确时间的位置。 */
export function formatAbsoluteTime(ms: number): string {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) {
    return "";
  }
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}`
  );
}

/** 中文相对时间；超过一周直接给绝对时间。 */
export function formatRelativeTime(ms: number, now: number = Date.now()): string {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) {
    return "";
  }
  const diff = now - ms;
  if (diff < 0) {
    return "刚刚";
  }
  if (diff < MINUTE) {
    return "刚刚";
  }
  if (diff < HOUR) {
    return `${Math.floor(diff / MINUTE)} 分钟前`;
  }
  if (diff < DAY) {
    return `${Math.floor(diff / HOUR)} 小时前`;
  }
  if (diff < 7 * DAY) {
    return `${Math.floor(diff / DAY)} 天前`;
  }
  return formatAbsoluteTime(ms);
}

const BYTE_UNITS = ["B", "KB", "MB", "GB", "TB"] as const;

/**
 * 字节数 → 人类可读（1024 进制，保留 1 位小数）。
 *
 * 界面上的「GB」就是 GiB（`storage.capacity_gb` 也按 GiB 计算），与后端口径一致；
 * 小于 1 KB 时给整数，避免出现「0.0 B」这种没有信息量的读数。
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return "—";
  }
  if (bytes < 1024) {
    return `${Math.round(bytes)} B`;
  }

  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // 1023.95 → 保留 1 位后是 1024.0，需要再进一位单位。
  if (Number(value.toFixed(1)) >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${BYTE_UNITS[unit]}`;
}
