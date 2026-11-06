/** 空库引导：告诉用户怎么产生第一条素材。 */
export function EmptyState() {
  return (
    <section className="empty">
      <div className="mark">
        <svg
          width="18"
          height="18"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.4"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <rect x="4.5" y="4.5" width="15" height="15" rx="3" />
          <path d="M12 9.2v5.6M9.2 12h5.6" />
        </svg>
      </div>
      <h3>还没有素材</h3>
      <p>按 Ctrl+Shift+S 或点击悬浮球开始捕获</p>
      <p>也可以把图片文件拖到悬浮球上导入。</p>
    </section>
  );
}
