/** 空库引导：告诉用户怎么产生第一条素材。 */
export function EmptyState() {
  return (
    <section className="library-empty">
      <h2>还没有素材</h2>
      <p>按 Ctrl+Shift+S 或点击悬浮球开始捕获</p>
      <p className="library-empty-hint">也可以把图片文件拖到悬浮球上导入。</p>
    </section>
  );
}
