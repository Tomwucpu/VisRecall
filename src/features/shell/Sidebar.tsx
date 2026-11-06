/**
 * 侧栏列骨架（T2 交付几何与边框，内容随 T6 接入）。
 *
 * T6 会在这里补：视图组（全部素材/标签）、标签云、存储条与折叠交互。
 * 当前只渲染分组标题，不放置任何未接线的控件。
 */
export function Sidebar() {
  return (
    <aside className="sidebar">
      <div className="sidebar-scroll">
        <div className="side-top">
          <span className="label">素材库</span>
        </div>
      </div>
    </aside>
  );
}
