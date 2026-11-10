import { type FileSelectionDto } from "../../lib/api";
import { formatBytes } from "../../lib/format";

type Props = {
  selection: FileSelectionDto;
  removedIds: ReadonlySet<string>;
  onRemove: (entryId: string) => void;
};

/** 待导入清单只展示摘要；不支持项保留原因，但不能提交。 */
export function ImportFileList({ selection, removedIds, onRemove }: Props) {
  return (
    <ul className="import-file-list" aria-label="待导入文件">
      {selection.files.map((file) => {
        const removed = removedIds.has(file.entryId);
        return (
          <li key={file.entryId} className={removed ? "is-removed" : undefined}>
            <div className="import-file-copy">
              <strong>{file.fileName}</strong>
              <span>
                {formatBytes(file.size)}
                {!file.supported && ` · ${file.reason ?? "不支持此文件类型"}`}
                {removed && " · 已移除"}
              </span>
            </div>
            {!removed && (
              <button type="button" className="icon-btn" aria-label={`移除 ${file.fileName}`} onClick={() => onRemove(file.entryId)}>
                <svg width="13" height="13" viewBox="0 0 14 14" stroke="currentColor" strokeWidth="1.4" aria-hidden="true">
                  <path d="M2 2l10 10M12 2L2 12" />
                </svg>
              </button>
            )}
          </li>
        );
      })}
    </ul>
  );
}
