//! Minimal platform-neutral vector math.
//!
//! The renderer uses Bevy's math types; the simulation deliberately keeps its
//! own small type so it stays independent of rendering crates.

use serde::{Deserialize, Serialize};

/// A 3D vector in world meters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Vec3 {
    /// East/west axis.
    pub x: f32,
    /// Up/down axis.
    pub y: f32,
    /// North/south axis.
    pub z: f32,
}

impl Vec3 {
    /// The origin.
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// Create a vector.
    #[must_use]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// Squared distance, avoiding a square root.
    #[must_use]
    pub fn distance_squared(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        dx * dx + dy * dy + dz * dz
    }

    /// Euclidean distance.
    #[must_use]
    pub fn distance(self, other: Self) -> f32 {
        self.distance_squared(other).sqrt()
    }

    /// Distance on the horizontal plane, ignoring height.
    ///
    /// Most range checks (interaction, chat, assistance) are horizontal so that
    /// stairs and small elevation changes do not break them.
    #[must_use]
    pub fn distance_xz(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dz = self.z - other.z;
        (dx * dx + dz * dz).sqrt()
    }

    /// Offset by another vector.
    #[must_use]
    pub fn translated(self, by: Self) -> Self {
        Self::new(self.x + by.x, self.y + by.y, self.z + by.z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_is_euclidean_and_xz_ignores_height() {
        let a = Vec3::new(0.0, 0.0, 0.0);
        let b = Vec3::new(3.0, 4.0, 0.0);
        assert!((a.distance(b) - 5.0).abs() < f32::EPSILON);
        let c = Vec3::new(3.0, 100.0, 4.0);
        assert!((a.distance_xz(c) - 5.0).abs() < 1e-5);
    }

    #[test]
    fn translation_composes() {
        let p = Vec3::new(1.0, 2.0, 3.0).translated(Vec3::new(-1.0, 0.5, 2.0));
        assert_eq!(p, Vec3::new(0.0, 2.5, 5.0));
    }
}
