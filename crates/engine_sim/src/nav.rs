//! Bounded, deterministic grid navigation.
//!
//! Pathfinding is A* over a uniform grid with octile distance, deterministic
//! tie-breaking, and an explicit expansion budget. See
//! `docs/adr/0010-navigation-and-pathfinding.md`.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use engine_core::math::Vec3;
use serde::{Deserialize, Serialize};

/// The largest accepted grid dimension.
pub const MAX_GRID_SIDE: usize = 4096;

/// Convert a cell coordinate into a float. Grid dimensions are capped at
/// [`MAX_GRID_SIDE`], which is far below `f32`'s exact-integer range.
#[allow(clippy::cast_precision_loss)]
fn cell_coordinate(value: usize) -> f32 {
    value as f32
}

/// Convert a floored, non-negative world coordinate into a cell index.
/// Out-of-grid results are rejected by the following bounds check.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn cell_index(value: f32) -> usize {
    value as usize
}

/// Work limits for one path request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathBudget {
    /// Maximum nodes expanded before returning [`PathError::BudgetExhausted`].
    pub max_expansions: usize,
    /// Maximum number of waypoints in a returned path.
    pub max_waypoints: usize,
}

impl Default for PathBudget {
    fn default() -> Self {
        Self {
            max_expansions: 4_096,
            max_waypoints: 512,
        }
    }
}

/// Why a path request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathError {
    /// The start or goal is outside the grid.
    OutOfBounds,
    /// The start or goal cell is blocked.
    Blocked,
    /// No path exists with the current obstacles.
    NoPath,
    /// The expansion or waypoint budget was exhausted.
    BudgetExhausted,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::OutOfBounds => "path endpoint is outside the navigation grid",
            Self::Blocked => "path endpoint is blocked",
            Self::NoPath => "no path exists",
            Self::BudgetExhausted => "pathfinding budget exhausted",
        })
    }
}

impl std::error::Error for PathError {}

/// A successful path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Path {
    /// Cell centers from start to goal, inclusive.
    pub waypoints: Vec<Vec3>,
    /// Number of nodes expanded.
    pub expansions: usize,
    /// Weighted movement cost.
    pub cost: u64,
}

impl Path {
    /// The goal waypoint, if any.
    #[must_use]
    pub fn goal(&self) -> Option<Vec3> {
        self.waypoints.last().copied()
    }
}

/// A uniform-cost navigation grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavGrid {
    width: usize,
    height: usize,
    cell_size: f32,
    origin: Vec3,
    blocked: Vec<bool>,
    cost: Vec<u16>,
}

impl NavGrid {
    /// Create an empty, fully walkable grid.
    ///
    /// # Errors
    ///
    /// Returns `None` when a dimension is zero or above [`MAX_GRID_SIDE`], the
    /// cell size is not finite and positive, or the origin is not finite.
    #[must_use]
    pub fn new(width: usize, height: usize, cell_size: f32, origin: Vec3) -> Option<Self> {
        if width == 0
            || height == 0
            || width > MAX_GRID_SIDE
            || height > MAX_GRID_SIDE
            || !cell_size.is_finite()
            || cell_size <= 0.0
            || !origin.x.is_finite()
            || !origin.y.is_finite()
            || !origin.z.is_finite()
        {
            return None;
        }
        let cells = width.checked_mul(height)?;
        Some(Self {
            width,
            height,
            cell_size,
            origin,
            blocked: vec![false; cells],
            cost: vec![1; cells],
        })
    }

    /// Grid width in cells.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Grid height in cells.
    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Edge length of one cell in world meters.
    #[must_use]
    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }

    /// World position of the grid's minimum corner.
    #[must_use]
    pub fn origin(&self) -> Vec3 {
        self.origin
    }

    /// World position of the grid's far corner (exclusive bound).
    #[must_use]
    pub fn extent(&self) -> Vec3 {
        Vec3::new(
            self.origin.x + cell_coordinate(self.width) * self.cell_size,
            self.origin.y,
            self.origin.z + cell_coordinate(self.height) * self.cell_size,
        )
    }

    /// Whether `(x, y)` is inside the grid.
    #[must_use]
    pub fn in_bounds(&self, x: usize, y: usize) -> bool {
        x < self.width && y < self.height
    }

    fn index(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }

    /// Whether a cell is blocked. Out-of-bounds cells count as blocked.
    #[must_use]
    pub fn is_blocked(&self, x: usize, y: usize) -> bool {
        if !self.in_bounds(x, y) {
            return true;
        }
        self.blocked[self.index(x, y)]
    }

    /// Whether a single step between adjacent grid cells is walkable.
    ///
    /// Both endpoints must be in bounds and open. Diagonal steps also require
    /// both orthogonal neighbours to be open. This is a cell-level primitive,
    /// not a continuous collision or entity-occupancy check.
    #[must_use]
    pub fn can_step(&self, from: (usize, usize), to: (usize, usize)) -> bool {
        let (x, y) = from;
        let (next_x, next_y) = to;
        let dx = x.abs_diff(next_x);
        let dy = y.abs_diff(next_y);
        if dx > 1
            || dy > 1
            || (dx == 0 && dy == 0)
            || self.is_blocked(x, y)
            || self.is_blocked(next_x, next_y)
        {
            return false;
        }
        dx == 0 || dy == 0 || (!self.is_blocked(next_x, y) && !self.is_blocked(x, next_y))
    }

    /// Block or unblock a cell.
    ///
    /// # Errors
    ///
    /// Returns `false` when the cell is outside the grid.
    pub fn set_blocked(&mut self, x: usize, y: usize, blocked: bool) -> bool {
        if !self.in_bounds(x, y) {
            return false;
        }
        let index = self.index(x, y);
        self.blocked[index] = blocked;
        true
    }

    /// Set a cell's movement cost multiplier. Values below one are clamped.
    ///
    /// # Errors
    ///
    /// Returns `false` when the cell is outside the grid.
    pub fn set_cost(&mut self, x: usize, y: usize, cost: u16) -> bool {
        if !self.in_bounds(x, y) {
            return false;
        }
        let index = self.index(x, y);
        self.cost[index] = cost.max(1);
        true
    }

    /// Movement cost multiplier for a cell.
    #[must_use]
    pub fn cost_at(&self, x: usize, y: usize) -> u16 {
        if self.in_bounds(x, y) {
            self.cost[self.index(x, y)]
        } else {
            1
        }
    }

    /// The world-space center of a cell.
    #[must_use]
    pub fn cell_center(&self, x: usize, y: usize) -> Vec3 {
        Vec3::new(
            self.origin.x + (cell_coordinate(x) + 0.5) * self.cell_size,
            self.origin.y,
            self.origin.z + (cell_coordinate(y) + 0.5) * self.cell_size,
        )
    }

    /// The cell containing a world position.
    #[must_use]
    pub fn cell_of(&self, position: Vec3) -> Option<(usize, usize)> {
        let x = ((position.x - self.origin.x) / self.cell_size).floor();
        let y = ((position.z - self.origin.z) / self.cell_size).floor();
        if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
            return None;
        }
        let x = cell_index(x);
        let y = cell_index(y);
        self.in_bounds(x, y).then_some((x, y))
    }

    /// Line-of-sight test using a grid traversal that cannot skip a cell.
    ///
    /// Endpoints outside the grid or inside blocked cells have no line of
    /// sight.
    #[must_use]
    pub fn line_of_sight(&self, from: Vec3, to: Vec3) -> bool {
        let Some((start_x, start_y)) = self.cell_of(from) else {
            return false;
        };
        let Some((goal_x, goal_y)) = self.cell_of(to) else {
            return false;
        };
        if self.is_blocked(start_x, start_y) || self.is_blocked(goal_x, goal_y) {
            return false;
        }

        let mut x = start_x;
        let mut y = start_y;
        if (x, y) == (goal_x, goal_y) {
            return true;
        }

        let dx = to.x - from.x;
        let dz = to.z - from.z;
        let step_x: isize = if dx > 0.0 {
            1
        } else if dx < 0.0 {
            -1
        } else {
            0
        };
        let step_y: isize = if dz > 0.0 {
            1
        } else if dz < 0.0 {
            -1
        } else {
            0
        };

        let mut t_max_x = match step_x.cmp(&0) {
            Ordering::Greater => {
                (self.origin.x + (cell_coordinate(x) + 1.0) * self.cell_size - from.x) / dx
            }
            Ordering::Less => {
                (from.x - (self.origin.x + cell_coordinate(x) * self.cell_size)) / -dx
            }
            Ordering::Equal => f32::INFINITY,
        };
        let mut t_max_y = match step_y.cmp(&0) {
            Ordering::Greater => {
                (self.origin.z + (cell_coordinate(y) + 1.0) * self.cell_size - from.z) / dz
            }
            Ordering::Less => {
                (from.z - (self.origin.z + cell_coordinate(y) * self.cell_size)) / -dz
            }
            Ordering::Equal => f32::INFINITY,
        };
        let t_delta_x = if step_x == 0 {
            f32::INFINITY
        } else {
            self.cell_size / dx.abs()
        };
        let t_delta_y = if step_y == 0 {
            f32::INFINITY
        } else {
            self.cell_size / dz.abs()
        };

        // The traversal crosses at most width + height cells.
        for _ in 0..(self.width + self.height + 2) {
            if t_max_x < t_max_y {
                t_max_x += t_delta_x;
                x = x.saturating_add_signed(step_x);
            } else {
                t_max_y += t_delta_y;
                y = y.saturating_add_signed(step_y);
            }
            if (x, y) == (goal_x, goal_y) {
                return true;
            }
            if !self.in_bounds(x, y) || self.is_blocked(x, y) {
                return false;
            }
        }
        false
    }

    /// Find a path between two world positions.
    ///
    /// # Errors
    ///
    /// Returns the most specific [`PathError`] for the failure.
    pub fn find_path(&self, from: Vec3, to: Vec3, budget: PathBudget) -> Result<Path, PathError> {
        let Some((start_x, start_y)) = self.cell_of(from) else {
            return Err(PathError::OutOfBounds);
        };
        let Some((goal_x, goal_y)) = self.cell_of(to) else {
            return Err(PathError::OutOfBounds);
        };
        if self.is_blocked(start_x, start_y) || self.is_blocked(goal_x, goal_y) {
            return Err(PathError::Blocked);
        }
        if (start_x, start_y) == (goal_x, goal_y) {
            return Ok(Path {
                waypoints: vec![self.cell_center(goal_x, goal_y)],
                expansions: 0,
                cost: 0,
            });
        }

        let cells = self.width * self.height;
        let start = self.index(start_x, start_y);
        let goal = self.index(goal_x, goal_y);
        let mut g_scores = vec![u64::MAX; cells];
        let mut came_from = vec![usize::MAX; cells];
        let mut open = BinaryHeap::new();
        let mut expansions = 0;

        g_scores[start] = 0;
        open.push(QueueNode {
            f: heuristic(start_x, start_y, goal_x, goal_y),
            h: heuristic(start_x, start_y, goal_x, goal_y),
            g: 0,
            index: start,
        });

        while let Some(current) = open.pop() {
            if current.index == goal {
                return self.reconstruct(goal, start, &came_from, &g_scores, budget, expansions);
            }
            if current.g > g_scores[current.index] {
                continue;
            }
            expansions += 1;
            if expansions > budget.max_expansions {
                return Err(PathError::BudgetExhausted);
            }

            let cx = current.index % self.width;
            let cy = current.index / self.width;
            for (dx, dy) in [
                (-1_i32, -1_i32),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ] {
                let Some(nx) = cx.checked_add_signed(dx as isize) else {
                    continue;
                };
                let Some(ny) = cy.checked_add_signed(dy as isize) else {
                    continue;
                };
                if !self.can_step((cx, cy), (nx, ny)) {
                    continue;
                }
                let diagonal = dx != 0 && dy != 0;
                let multiplier = u64::from(self.cost_at(nx, ny));
                let step = multiplier * if diagonal { 14 } else { 10 };
                let tentative = current.g.saturating_add(step);
                let neighbour = self.index(nx, ny);
                if tentative < g_scores[neighbour] {
                    came_from[neighbour] = current.index;
                    g_scores[neighbour] = tentative;
                    let h = heuristic(nx, ny, goal_x, goal_y);
                    open.push(QueueNode {
                        f: tentative.saturating_add(h),
                        h,
                        g: tentative,
                        index: neighbour,
                    });
                }
            }
        }

        Err(PathError::NoPath)
    }

    fn reconstruct(
        &self,
        goal: usize,
        start: usize,
        came_from: &[usize],
        g_scores: &[u64],
        budget: PathBudget,
        expansions: usize,
    ) -> Result<Path, PathError> {
        let mut indices = vec![goal];
        let mut current = goal;
        while current != start {
            let previous = came_from[current];
            if previous == usize::MAX {
                return Err(PathError::NoPath);
            }
            indices.push(previous);
            current = previous;
            if indices.len() > budget.max_waypoints {
                return Err(PathError::BudgetExhausted);
            }
        }
        indices.reverse();
        let waypoints = indices
            .into_iter()
            .map(|index| self.cell_center(index % self.width, index / self.width))
            .collect();
        Ok(Path {
            waypoints,
            expansions,
            cost: g_scores[goal],
        })
    }
}

fn heuristic(x: usize, y: usize, goal_x: usize, goal_y: usize) -> u64 {
    let dx = x.abs_diff(goal_x) as u64;
    let dy = y.abs_diff(goal_y) as u64;
    let (max, min) = if dx > dy { (dx, dy) } else { (dy, dx) };
    10 * max + 4 * min
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueueNode {
    f: u64,
    h: u64,
    g: u64,
    index: usize,
}

impl Ord for QueueNode {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering for a min-heap. Prefer lower f, then lower h, then
        // lower g, then the lower cell index for deterministic ties.
        other
            .f
            .cmp(&self.f)
            .then_with(|| other.h.cmp(&self.h))
            .then_with(|| other.g.cmp(&self.g))
            .then_with(|| other.index.cmp(&self.index))
    }
}

impl PartialOrd for QueueNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(width: usize, height: usize) -> NavGrid {
        NavGrid::new(width, height, 1.0, Vec3::ZERO).unwrap()
    }

    fn at(x: f32, z: f32) -> Vec3 {
        Vec3::new(x, 0.0, z)
    }

    #[test]
    fn grids_reject_invalid_dimensions() {
        assert!(NavGrid::new(0, 4, 1.0, Vec3::ZERO).is_none());
        assert!(NavGrid::new(4, 4, 0.0, Vec3::ZERO).is_none());
        assert!(NavGrid::new(4, 4, f32::NAN, Vec3::ZERO).is_none());
        assert!(NavGrid::new(MAX_GRID_SIDE + 1, 1, 1.0, Vec3::ZERO).is_none());
    }

    #[test]
    fn cells_map_to_world_positions() {
        let room = NavGrid::new(4, 3, 2.0, Vec3::new(10.0, 1.0, 20.0)).unwrap();
        assert_eq!(room.cell_of(Vec3::new(10.5, 0.0, 20.5)), Some((0, 0)));
        assert_eq!(room.cell_of(Vec3::new(13.5, 0.0, 25.5)), Some((1, 2)));
        assert_eq!(room.cell_of(Vec3::new(9.9, 0.0, 20.0)), None);
        assert_eq!(room.cell_center(1, 0), Vec3::new(13.0, 1.0, 21.0));
    }

    #[test]
    fn straight_paths_are_shortest() {
        let room = grid(8, 8);
        let path = room
            .find_path(at(0.5, 0.5), at(5.5, 0.5), PathBudget::default())
            .unwrap();
        assert_eq!(path.waypoints.first(), Some(&at(0.5, 0.5)));
        assert_eq!(path.goal(), Some(at(5.5, 0.5)));
        assert_eq!(path.cost, 50);
        assert_eq!(path.waypoints.len(), 6);
    }

    #[test]
    fn paths_route_around_walls() {
        let mut room = grid(8, 8);
        for y in 0..7 {
            room.set_blocked(4, y, true);
        }
        let path = room
            .find_path(at(0.5, 0.5), at(7.5, 0.5), PathBudget::default())
            .unwrap();
        assert!(path.waypoints.iter().any(|point| point.z > 7.0));
        assert!(path.cost > 70);
        assert!(path.waypoints.iter().all(|point| {
            room.cell_of(*point)
                .is_some_and(|(x, y)| !room.is_blocked(x, y))
        }));
    }

    #[test]
    fn no_path_is_reported_when_walled_in() {
        let mut room = grid(5, 5);
        for x in 0..5 {
            room.set_blocked(x, 2, true);
        }
        assert_eq!(
            room.find_path(at(2.5, 0.5), at(2.5, 4.5), PathBudget::default()),
            Err(PathError::NoPath)
        );
    }

    #[test]
    fn diagonal_corner_cutting_is_forbidden() {
        let mut room = grid(3, 3);
        room.set_blocked(1, 0, true);
        let path = room
            .find_path(at(0.5, 0.5), at(1.5, 1.5), PathBudget::default())
            .unwrap();
        // A diagonal corner cut would cost 14; the legal route costs 20.
        assert_eq!(path.cost, 20);
    }

    #[test]
    fn cell_steps_fail_closed_and_cannot_cut_either_corner() {
        let mut room = grid(3, 3);
        assert!(room.can_step((0, 0), (1, 0)));
        assert!(room.can_step((0, 0), (1, 1)));
        assert!(!room.can_step((0, 0), (0, 0)));
        assert!(!room.can_step((0, 0), (2, 0)));
        assert!(!room.can_step((0, 0), (3, 0)));
        assert!(!room.can_step((usize::MAX, 0), (0, 0)));

        room.set_blocked(1, 0, true);
        assert!(!room.can_step((0, 0), (1, 0)));
        assert!(!room.can_step((1, 0), (0, 0)));
        assert!(!room.can_step((0, 0), (1, 1)));
        room.set_blocked(1, 0, false);
        room.set_blocked(0, 1, true);
        assert!(!room.can_step((0, 0), (1, 1)));
        room.set_blocked(0, 1, false);
        room.set_blocked(1, 1, true);
        assert!(!room.can_step((0, 0), (1, 1)));
    }

    #[test]
    fn budgets_are_enforced() {
        let room = grid(256, 256);
        let tight = PathBudget {
            max_expansions: 2,
            max_waypoints: 512,
        };
        assert_eq!(
            room.find_path(at(0.5, 0.5), at(255.5, 255.5), tight),
            Err(PathError::BudgetExhausted)
        );
    }

    #[test]
    fn line_of_sight_respects_walls() {
        let mut room = grid(8, 8);
        assert!(room.line_of_sight(at(0.5, 0.5), at(7.5, 7.5)));
        for y in 0..8 {
            room.set_blocked(4, y, true);
        }
        assert!(!room.line_of_sight(at(0.5, 0.5), at(7.5, 0.5)));
        assert!(room.line_of_sight(at(0.5, 0.5), at(3.5, 0.5)));
    }

    #[test]
    fn out_of_bounds_and_blocked_endpoints_are_reported() {
        let mut room = grid(4, 4);
        assert_eq!(
            room.find_path(at(-1.0, 0.5), at(1.5, 1.5), PathBudget::default()),
            Err(PathError::OutOfBounds)
        );
        room.set_blocked(1, 1, true);
        assert_eq!(
            room.find_path(at(1.5, 1.5), at(2.5, 2.5), PathBudget::default()),
            Err(PathError::Blocked)
        );
    }

    #[test]
    fn weighted_cells_cost_more() {
        let mut room = grid(4, 4);
        room.set_cost(1, 0, 50);
        let path = room
            .find_path(at(0.5, 0.5), at(3.5, 0.5), PathBudget::default())
            .unwrap();
        // The direct route pays the weighted cell; a short diagonal detour wins.
        assert_eq!(path.cost, 38);
    }
}
