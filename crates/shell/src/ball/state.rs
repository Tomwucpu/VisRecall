//! 悬浮球三态状态机（design §6.10）。
//!
//! 这里**不碰窗口、鼠标或定时器**：所有时间来自事件里的 `now_ms`，所有输出是
//! [`BallAction`]，由 `ball::mod` 的消息循环负责落地。这样贴边/收起/展开/全屏
//! 这一套时序可以在单测里跳时间验证，不需要真的去拖一个分层窗口。
//!
//! 三条规则值得单独记住：
//! 1. 贴边判定用**松手位置到工作区边缘的距离**（≤12px），不是球心；
//! 2. 收起需要静置 800ms，且期间鼠标不能停在热区里——「拖到边上接着想点它」
//!    不该先收起来再弹回来；
//! 3. 全屏隐藏要连续 3 次采样命中才生效：单次误判（游戏全屏切换、远程桌面）
//!    会让球闪一下又回来，观感比不隐藏更差。

use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};

use crate::ball::anim::BallMetrics;

/// 原型 56px 基准下的距边阈值；实际值由 [`BallMetrics`] 按尺寸缩放。
pub const EDGE_SNAP_PX: i32 = 64;

/// 松手后静置多久收成胶囊条。
pub const COLLAPSE_DELAY_MS: u64 = 800;

/// 鼠标离开后多久回缩。
pub const RETRACT_DELAY_MS: u64 = 800;

/// 全屏检测连续命中多少次才隐藏。
pub const FULLSCREEN_TICKS: u8 = 3;

/// 悬浮球尺寸范围（逻辑像素）；设置页滑动条与配置校验共用这一口径。
pub const BALL_MIN_SIZE: u32 = 50;
pub const BALL_MAX_SIZE: u32 = 150;

/// 显示形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BallVisual {
    Ball,
    Strip,
    Hidden,
}

/// 贴边方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BallEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl BallEdge {
    /// 左右贴边收成竖直长条，上下贴边收成水平长条。
    pub fn is_vertical(self) -> bool {
        matches!(self, BallEdge::Left | BallEdge::Right)
    }
}

/// 状态机需要的配置子集。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BallSettings {
    pub size: u32,
    pub edge_hide: bool,
    pub hide_in_fullscreen: bool,
}

impl Default for BallSettings {
    fn default() -> Self {
        Self {
            size: 48,
            edge_hide: true,
            hide_in_fullscreen: true,
        }
    }
}

/// 输入事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BallEvent {
    /// 越过拖动阈值：脱锚，回到自由球形态（点击不触发）。
    DragStarted,
    /// 拖动中：球当前左上角。
    Dragged { position: PhysicalPoint },
    /// 松手：位置 + 所在显示器工作区。
    Released {
        position: PhysicalPoint,
        work_area: PhysicalRect,
        now_ms: u64,
    },
    /// 鼠标进入热区。
    CursorEnter { now_ms: u64 },
    /// 鼠标离开热区。
    CursorLeave { now_ms: u64 },
    /// 定时采样：`fullscreen` 由 `SHQueryUserNotificationState` 判定。
    Tick { now_ms: u64, fullscreen: bool },
    /// 设置变更（尺寸/贴边/全屏隐藏）。
    ConfigChanged { settings: BallSettings },
    /// 捕获会话覆盖可见性：`true` 时隐藏，`false` 时按当前形态恢复。
    SessionHidden(bool),
}

/// 状态机产出的动作，由窗口层执行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BallAction {
    /// 以该矩形显示（球 = size×size；长条 = 5 + 两侧留白），必要时移动/改尺寸并重绘。
    Show { rect: PhysicalRect },
    /// 隐藏窗口（全屏或捕获会话）。
    Hide,
    /// 尺寸热更新：保持中心点不变。
    Resize { size: u32, center: PhysicalPoint },
    /// 需要一个定时器在这之后唤醒（窗口层只保留最近的超时）。
    ScheduleTimer { after_ms: u64 },
}

/// 三态状态机。
#[derive(Clone, Debug)]
pub struct BallStateMachine {
    settings: BallSettings,
    visual: BallVisual,
    /// 球心（物理像素）——所有形态都以它为中心，收起/展开不会让球漂移。
    center: PhysicalPoint,
    work_area: PhysicalRect,
    anchor: Option<BallEdge>,
    hover: bool,
    session_hidden: bool,
    collapse_at: Option<u64>,
    retract_at: Option<u64>,
    fullscreen_streak: u8,
}

impl BallStateMachine {
    /// `center` 是球心；`work_area` 是球所在显示器的工作区。
    pub fn new(
        settings: BallSettings,
        center: PhysicalPoint,
        work_area: PhysicalRect,
        _now_ms: u64,
    ) -> Self {
        let size = clamp_size(settings.size);
        let mut machine = Self {
            settings: BallSettings { size, ..settings },
            visual: BallVisual::Ball,
            center,
            work_area,
            anchor: None,
            hover: false,
            session_hidden: false,
            collapse_at: None,
            retract_at: None,
            fullscreen_streak: 0,
        };
        machine.center = machine.clamp_center(center, size);
        machine
    }

    pub fn visual(&self) -> BallVisual {
        self.visual
    }

    pub fn size(&self) -> u32 {
        self.settings.size
    }

    pub fn settings(&self) -> BallSettings {
        self.settings
    }

    /// 工作区变化（换屏、任务栏改位置）：夹回中心点，不改变形态。
    pub fn set_work_area(&mut self, work_area: PhysicalRect) {
        self.work_area = work_area;
        self.center = self.clamp_center(self.center, self.settings.size);
    }

    pub fn center(&self) -> PhysicalPoint {
        self.center
    }

    pub fn anchor(&self) -> Option<BallEdge> {
        self.anchor
    }

    /// 当前形态对应的视觉矩形（不含窗口阴影/命中留白）。
    pub fn current_rect(&self) -> PhysicalRect {
        match self.visual {
            BallVisual::Strip => self.strip_rect(),
            BallVisual::Ball if self.anchor.is_some() => self.docked_ball_rect(),
            BallVisual::Ball | BallVisual::Hidden => self.ball_rect(),
        }
    }

    /// 自由球形态的矩形（以中心点为锚）。
    pub fn ball_rect(&self) -> PhysicalRect {
        let size = self.settings.size;
        PhysicalRect::new(
            self.center.x - size as i32 / 2,
            self.center.y - size as i32 / 2,
            size,
            size,
        )
    }

    /// 贴边展开态：球心向内让出 12px，避免贴死在边缘。
    pub fn docked_ball_rect(&self) -> PhysicalRect {
        let metrics = BallMetrics::new(self.settings.size);
        let size = metrics.size as i32;
        let offset = metrics.edge_offset as i32;
        let (x, y) = match self.anchor {
            Some(BallEdge::Left) => (self.work_area.x + offset, self.center.y - size / 2),
            Some(BallEdge::Right) => (
                self.work_area.right() - offset - size,
                self.center.y - size / 2,
            ),
            Some(BallEdge::Top) => (self.center.x - size / 2, self.work_area.y + offset),
            Some(BallEdge::Bottom) => (
                self.center.x - size / 2,
                self.work_area.bottom() - offset - size,
            ),
            None => return self.ball_rect(),
        };
        // 工作区比球还小时，`max` 兜底保证 clamp 的上下界有序。
        let max_x = (self.work_area.right() - size).max(self.work_area.x);
        let max_y = (self.work_area.bottom() - size).max(self.work_area.y);
        PhysicalRect::new(
            x.clamp(self.work_area.x, max_x),
            y.clamp(self.work_area.y, max_y),
            metrics.size,
            metrics.size,
        )
    }

    /// 收起胶囊的视觉矩形：10px 厚、球径 + 6 长，贴住工作区边缘。
    pub fn strip_rect(&self) -> PhysicalRect {
        let metrics = BallMetrics::new(self.settings.size);
        let thickness = metrics.strip_thickness;
        let length = metrics.strip_length;
        match self.anchor {
            Some(edge) if edge.is_vertical() => {
                let x = match edge {
                    BallEdge::Left => self.work_area.x,
                    _ => self.work_area.right() - thickness as i32,
                };
                let max_y = (self.work_area.bottom() - length as i32).max(self.work_area.y);
                let y = (self.center.y - length as i32 / 2).clamp(self.work_area.y, max_y);
                PhysicalRect::new(x, y, thickness, length)
            }
            Some(edge) => {
                let y = match edge {
                    BallEdge::Top => self.work_area.y,
                    _ => self.work_area.bottom() - thickness as i32,
                };
                let max_x = (self.work_area.right() - length as i32).max(self.work_area.x);
                let x = (self.center.x - length as i32 / 2).clamp(self.work_area.x, max_x);
                PhysicalRect::new(x, y, length, thickness)
            }
            // 没有贴边锚点时不存在胶囊形态；退回球矩形保持几何自洽。
            None => self.ball_rect(),
        }
    }

    /// 推进一个事件，返回需要执行的动作。
    pub fn handle(&mut self, event: BallEvent) -> Vec<BallAction> {
        match event {
            BallEvent::DragStarted => {
                self.anchor = None;
                self.collapse_at = None;
                self.retract_at = None;
                if self.visual == BallVisual::Strip {
                    self.visual = BallVisual::Ball;
                }
                vec![BallAction::Show {
                    rect: self.current_rect(),
                }]
            }
            BallEvent::Dragged { position } => {
                // 拖动中只更新中心点，不改形态（长条在拖动中不该出现）。
                self.center = PhysicalPoint {
                    x: position.x + self.settings.size as i32 / 2,
                    y: position.y + self.settings.size as i32 / 2,
                };
                Vec::new()
            }
            BallEvent::Released {
                position,
                work_area,
                now_ms,
            } => self.on_released(position, work_area, now_ms),
            BallEvent::CursorEnter { now_ms } => self.on_cursor_enter(now_ms),
            BallEvent::CursorLeave { now_ms } => self.on_cursor_leave(now_ms),
            BallEvent::Tick { now_ms, fullscreen } => self.on_tick(now_ms, fullscreen),
            BallEvent::ConfigChanged { settings } => self.on_config_changed(settings),
            BallEvent::SessionHidden(hidden) => {
                self.session_hidden = hidden;
                if hidden {
                    vec![BallAction::Hide]
                } else if self.visual == BallVisual::Hidden {
                    // 全屏仍在时保持隐藏，会话结束不越权把球叫回来。
                    Vec::new()
                } else {
                    vec![BallAction::Show {
                        rect: self.current_rect(),
                    }]
                }
            }
        }
    }

    fn on_released(
        &mut self,
        position: PhysicalPoint,
        work_area: PhysicalRect,
        now_ms: u64,
    ) -> Vec<BallAction> {
        self.work_area = work_area;
        let size = self.settings.size;
        let center = PhysicalPoint {
            x: position.x + size as i32 / 2,
            y: position.y + size as i32 / 2,
        };
        self.center = self.clamp_center(center, size);

        let edge = if self.settings.edge_hide {
            nearest_edge(position, work_area, size)
        } else {
            None
        };

        let mut actions = vec![BallAction::Show {
            rect: self.ball_rect(),
        }];

        match edge {
            Some(edge) => {
                self.anchor = Some(edge);
                // 贴边后的中心点是内缩 12px 的展开位置，避免拖拽起点与绘制位置不一致。
                self.center = self.docked_center(edge);
                // 已有形态是长条时（贴边状态下重新拖到边上）直接从长条算起。
                self.collapse_at = Some(now_ms + COLLAPSE_DELAY_MS);
                actions.push(BallAction::ScheduleTimer {
                    after_ms: COLLAPSE_DELAY_MS,
                });
            }
            None => {
                self.anchor = None;
                self.collapse_at = None;
                if self.visual == BallVisual::Strip {
                    // 拖离边缘：立即恢复球形态。
                    self.visual = BallVisual::Ball;
                    actions = vec![BallAction::Show {
                        rect: self.ball_rect(),
                    }];
                }
            }
        }

        actions
    }

    fn on_cursor_enter(&mut self, _now_ms: u64) -> Vec<BallAction> {
        self.hover = true;
        // 悬停会取消待执行的收起：用户显然还想用它。
        self.collapse_at = None;
        if self.visual == BallVisual::Strip {
            self.visual = BallVisual::Ball;
            if let Some(edge) = self.anchor {
                self.center = self.docked_center(edge);
            }
            return vec![BallAction::Show {
                rect: self.current_rect(),
            }];
        }
        Vec::new()
    }

    /// 贴边展开态的中心点：距工作区边缘 `edge_offset` 再加半个球。
    fn docked_center(&self, edge: BallEdge) -> PhysicalPoint {
        let metrics = BallMetrics::new(self.settings.size);
        let half = metrics.size as i32 / 2;
        let offset = metrics.edge_offset as i32;
        match edge {
            BallEdge::Left => PhysicalPoint {
                x: self.work_area.x + offset + half,
                y: self.center.y,
            },
            BallEdge::Right => PhysicalPoint {
                x: self.work_area.right() - offset - half,
                y: self.center.y,
            },
            BallEdge::Top => PhysicalPoint {
                x: self.center.x,
                y: self.work_area.y + offset + half,
            },
            BallEdge::Bottom => PhysicalPoint {
                x: self.center.x,
                y: self.work_area.bottom() - offset - half,
            },
        }
    }

    fn on_cursor_leave(&mut self, now_ms: u64) -> Vec<BallAction> {
        self.hover = false;
        if self.visual == BallVisual::Ball && self.anchor.is_some() {
            self.retract_at = Some(now_ms + RETRACT_DELAY_MS);
            return vec![BallAction::ScheduleTimer {
                after_ms: RETRACT_DELAY_MS,
            }];
        }
        Vec::new()
    }

    fn on_tick(&mut self, now_ms: u64, fullscreen: bool) -> Vec<BallAction> {
        if fullscreen && self.settings.hide_in_fullscreen {
            self.fullscreen_streak = self.fullscreen_streak.saturating_add(1);
            if self.fullscreen_streak >= FULLSCREEN_TICKS && self.visual != BallVisual::Hidden {
                self.visual = BallVisual::Hidden;
                return vec![BallAction::Hide];
            }
            return Vec::new();
        }

        self.fullscreen_streak = 0;
        if self.visual == BallVisual::Hidden {
            // 退出全屏：回到贴边决定的形态（没有锚点就是球）。
            self.visual = if self.anchor.is_some() {
                BallVisual::Strip
            } else {
                BallVisual::Ball
            };
            if self.session_hidden {
                return Vec::new();
            }
            return vec![BallAction::Show {
                rect: self.current_rect(),
            }];
        }

        // 收起：到点且鼠标不在热区里。
        if let Some(deadline) = self.collapse_at {
            if now_ms >= deadline {
                self.collapse_at = None;
                if !self.hover && self.anchor.is_some() && self.visual == BallVisual::Ball {
                    self.visual = BallVisual::Strip;
                    return vec![BallAction::Show {
                        rect: self.strip_rect(),
                    }];
                }
            }
        }

        // 回缩：鼠标离开 800ms 且仍贴边。
        if let Some(deadline) = self.retract_at {
            if now_ms >= deadline {
                self.retract_at = None;
                if !self.hover && self.anchor.is_some() && self.visual == BallVisual::Ball {
                    self.visual = BallVisual::Strip;
                    return vec![BallAction::Show {
                        rect: self.strip_rect(),
                    }];
                }
            }
        }

        Vec::new()
    }

    fn on_config_changed(&mut self, settings: BallSettings) -> Vec<BallAction> {
        let new_size = clamp_size(settings.size);
        let size_changed = new_size != self.settings.size;
        let edge_hide_was = self.settings.edge_hide;
        let fullscreen_was = self.settings.hide_in_fullscreen;

        self.settings = BallSettings {
            size: new_size,
            ..settings
        };

        let mut actions = Vec::new();

        if size_changed {
            // 球变大后可能越界：中心点夹回工作区，再按新尺寸重算矩形。
            let center = self.clamp_center(self.center, new_size);
            self.center = center;
            actions.push(BallAction::Resize {
                size: new_size,
                center,
            });
        }

        if edge_hide_was && !self.settings.edge_hide {
            // 关掉贴边收起：长条立刻变回球，锚点清空（否则下次松手会直接收）。
            self.anchor = None;
            self.collapse_at = None;
            self.retract_at = None;
            if self.visual == BallVisual::Strip {
                self.visual = BallVisual::Ball;
            }
        }

        if !fullscreen_was && !self.settings.hide_in_fullscreen {
            self.fullscreen_streak = 0;
            if self.visual == BallVisual::Hidden {
                self.visual = BallVisual::Ball;
            }
        }

        if size_changed || self.visual != BallVisual::Hidden {
            actions.push(BallAction::Show {
                rect: self.current_rect(),
            });
        }
        actions
    }

    /// 把中心点夹到「整个球都在工作区内」的范围。
    fn clamp_center(&self, center: PhysicalPoint, size: u32) -> PhysicalPoint {
        let half = size as i32 / 2;
        let min_x = self.work_area.x + half;
        let max_x = self.work_area.right() - (size as i32 - half);
        let min_y = self.work_area.y + half;
        let max_y = self.work_area.bottom() - (size as i32 - half);

        PhysicalPoint {
            x: center.x.clamp(min_x.min(max_x), max_x.max(min_x)),
            y: center.y.clamp(min_y.min(max_y), max_y.max(min_y)),
        }
    }
}

/// 尺寸夹到 50..=150（配置被手改坏时的兜底）。
pub fn clamp_size(size: u32) -> u32 {
    size.clamp(BALL_MIN_SIZE, BALL_MAX_SIZE)
}

/// 松手位置距哪条边在阈值以内；同时贴近两条边时取更近的一条。
pub fn nearest_edge(
    position: PhysicalPoint,
    work_area: PhysicalRect,
    size: u32,
) -> Option<BallEdge> {
    let threshold = BallMetrics::new(size).threshold;
    let left = position.x - work_area.x;
    let right = work_area.right() - (position.x + size as i32);
    let top = position.y - work_area.y;
    let bottom = work_area.bottom() - (position.y + size as i32);

    let candidates = [
        (BallEdge::Left, left),
        (BallEdge::Right, right),
        (BallEdge::Top, top),
        (BallEdge::Bottom, bottom),
    ];

    candidates
        .into_iter()
        // 距离为负说明球已经越出工作区（拖动中可能短暂出现），不算贴边。
        .filter(|(_, distance)| (0..=threshold).contains(distance))
        .min_by_key(|(_, distance)| *distance)
        .map(|(edge, _)| edge)
}
