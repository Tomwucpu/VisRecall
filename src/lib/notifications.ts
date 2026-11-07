/*
 * 通知类事件 → Toast。
 *
 * 这些事件不改变查询数据（不失效缓存），只负责让「后台发生的事」在界面上可见：
 * 捕获失败、快捷键冲突、崩溃后任务恢复。与 `events.ts` 的查询失效职责分开，
 * 避免把「刷新数据」和「提示用户」混在一条分支里。
 */
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { pushToast } from "./toast";

/** 这些失败码属于用户预期内（自己取消）或后台可自愈（缩略图重排），不打扰。 */
const SILENT_CODES = new Set(["CAPTURE_CANCELLED", "THUMBNAIL_FAILED"]);

/** 稳定错误码 → 面向用户的文案；`message` 是内部英文细节，不直接展示。 */
const CODE_TEXT: Record<string, string> = {
  BUSY: "正在迁移或清理库，暂时无法捕获",
  CAPTURE_FAILED: "截图失败，请重试",
  CAPTURE_UNAVAILABLE: "捕获服务未就绪，请重启应用",
  CLIPBOARD_BUSY: "剪贴板被其他程序占用，请重试",
  CLIPBOARD_FAILED: "写入剪贴板失败",
  CONFIG_MISSING: "库未就绪，请检查存储设置",
  DRM_PROTECTED: "当前画面受版权保护，无法截取",
  DROP_REJECTED: "不支持的文件类型（支持 png/jpg/webp/gif/txt/md）",
  FILE_MISSING: "文件缺失，操作未完成",
  INVALID_INPUT: "内容无法导入",
  NOT_FOUND: "素材已不存在",
  STORAGE_FULL: "存储已满，请清理或调整容量",
};

function textOf(payload: unknown): string | null {
  if (typeof payload === "object" && payload !== null) {
    const message = (payload as { message?: unknown }).message;
    if (typeof message === "string" && message !== "") {
      return message;
    }
  }
  return null;
}

/** 把 `capture_failed` 载荷翻成一句提示；无声码返回 `null`。 */
export function describeCaptureFailure(payload: unknown): string | null {
  if (typeof payload !== "object" || payload === null) {
    return "捕获失败";
  }
  const { code } = payload as { code?: unknown };
  if (typeof code !== "string" || SILENT_CODES.has(code)) {
    return null;
  }
  return CODE_TEXT[code] ?? `捕获失败（${code}）`;
}

/**
 * 订阅通知类事件，返回清理函数。
 *
 * 与 `subscribeAppEvents` 同样的「先标记 disposed、再在途退订」写法，
 * 保证 StrictMode 的重复挂载不会留下重复提示。
 */
export function subscribeNotifications(): () => void {
  let disposed = false;
  const unlisteners: UnlistenFn[] = [];

  const bind = (name: string, handle: (payload: unknown) => void) => {
    listen(name, (event) => {
      if (!disposed) {
        handle(event.payload);
      }
    })
      .then((unlisten) => {
        if (disposed) {
          unlisten();
        } else {
          unlisteners.push(unlisten);
        }
      })
      .catch((error: unknown) => {
        console.error(`listen ${name} failed`, error);
      });
  };

  bind("capture_failed", (payload) => {
    const text = describeCaptureFailure(payload);
    if (text !== null) {
      pushToast(text, "error");
    }
  });

  bind("hotkey_conflict", (payload) => {
    const message = textOf(payload);
    pushToast(message === null ? "捕获快捷键不可用" : `快捷键不可用：${message}`, "error");
  });

  bind("app_recovered", (payload) => {
    const count =
      typeof payload === "object" && payload !== null
        ? (payload as { count?: unknown }).count
        : null;
    if (typeof count === "number" && count > 0) {
      pushToast(`已恢复 ${count} 个未完成任务`, "info");
    }
  });

  return () => {
    disposed = true;
    for (const unlisten of unlisteners.splice(0)) {
      unlisten();
    }
  };
}
