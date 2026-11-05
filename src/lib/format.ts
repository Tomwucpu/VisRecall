import { convertFileSrc } from "@tauri-apps/api/core";

const THUMB_PREFIX = "visrecall://thumb/";

/**
 * `ItemDto.thumbUrl`（`visrecall://thumb/<id>`）→ 自定义协议的可加载地址。
 *
 * `convertFileSrc` 会把整段路径当成文件路径做 `encodeURIComponent`，冒号/斜杠会被
 * 编码成 `%3A`/`%2F`，协议处理器就只看到一段路径了。所以这里只交给它固定的
 * `thumb` 段，再由自己拼 `/<id>`。非 `visrecall://` 的值原样返回。
 */
export function toThumbSrc(url: string | null): string | null {
  if (!url) {
    return null;
  }
  if (!url.startsWith(THUMB_PREFIX)) {
    return url;
  }
  const id = url.slice(THUMB_PREFIX.length);
  return `${convertFileSrc("thumb", "visrecall")}/${encodeURIComponent(id)}`;
}

/** 状态徽标文案（design §11.1：parsing/ready/pending/failed）。 */
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
      return status;
  }
}

/** 素材类型文案。 */
export function kindLabel(kind: string): string {
  switch (kind) {
    case "image":
      return "图片";
    case "text":
      return "文字";
    case "link":
      return "链接";
    default:
      return kind;
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
