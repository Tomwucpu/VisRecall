/** 命令错误统一为 `{ code, message }`（见 `ApiError`）。 */

/** 面向用户的一句话：只取 `message`，不把错误码塞进提示。 */
export function errorMessage(error: unknown): string {
  if (typeof error === "object" && error !== null) {
    const { message } = error as { message?: unknown };
    if (typeof message === "string") {
      return message;
    }
  }
  return error instanceof Error ? error.message : String(error);
}

/** 取稳定错误码；用于按 `FILE_MISSING` / `CLIPBOARD_BUSY` 等分支给不同提示。 */
export function errorCode(error: unknown): string | null {
  if (typeof error === "object" && error !== null) {
    const { code } = error as { code?: unknown };
    if (typeof code === "string") {
      return code;
    }
  }
  return null;
}

/** 面向排查：`CODE: message`，用在错误卡片等需要定位问题的位置。 */
export function describeError(error: unknown): string {
  if (typeof error === "object" && error !== null) {
    const { code, message } = error as { code?: unknown; message?: unknown };
    if (typeof message === "string") {
      return typeof code === "string" ? `${code}: ${message}` : message;
    }
  }
  return error instanceof Error ? error.message : String(error);
}
