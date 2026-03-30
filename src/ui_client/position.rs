use std::fs;
use std::path::{Path, PathBuf};

use gpui::{Bounds, Pixels, Point, Size, point};
use serde::{Deserialize, Serialize};

const WINDOW_POSITION_FILE: &str = "transcribe-ui-position.json";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NormalizedPosition {
    pub x: f32,
    pub y: f32,
}

impl NormalizedPosition {
    pub fn clamped(self) -> Self {
        Self {
            x: self.x.clamp(0.0, 1.0),
            y: self.y.clamp(0.0, 1.0),
        }
    }
}

pub fn position_file_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(WINDOW_POSITION_FILE)
}

pub fn load_position(runtime_dir: &Path) -> Option<NormalizedPosition> {
    let path = position_file_path(runtime_dir);
    let raw = fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<NormalizedPosition>(&raw).ok()?;
    Some(parsed.clamped())
}

pub fn save_position(runtime_dir: &Path, position: NormalizedPosition) {
    let path = position_file_path(runtime_dir);
    let _ = fs::create_dir_all(runtime_dir);
    let value = position.clamped();
    if let Ok(json) = serde_json::to_string_pretty(&value) {
        let _ = fs::write(path, json);
    }
}

pub fn normalize_center(
    window_bounds: Bounds<Pixels>,
    visible_bounds: Bounds<Pixels>,
) -> Option<NormalizedPosition> {
    let width = f32::from(visible_bounds.size.width);
    let height = f32::from(visible_bounds.size.height);
    if width <= 1.0 || height <= 1.0 {
        return None;
    }

    let center = window_bounds.center();
    let x = (f32::from(center.x - visible_bounds.origin.x) / width).clamp(0.0, 1.0);
    let y = (f32::from(center.y - visible_bounds.origin.y) / height).clamp(0.0, 1.0);

    Some(NormalizedPosition { x, y })
}

pub fn bounds_from_normalized(
    visible_bounds: Bounds<Pixels>,
    window_size: Size<Pixels>,
    normalized: NormalizedPosition,
) -> Bounds<Pixels> {
    let normalized = normalized.clamped();

    let vw = f32::from(visible_bounds.size.width).max(1.0);
    let vh = f32::from(visible_bounds.size.height).max(1.0);

    let center_x = f32::from(visible_bounds.origin.x) + normalized.x * vw;
    let center_y = f32::from(visible_bounds.origin.y) + normalized.y * vh;

    let mut origin_x = center_x - f32::from(window_size.width) * 0.5;
    let mut origin_y = center_y - f32::from(window_size.height) * 0.5;

    let min_x = f32::from(visible_bounds.origin.x);
    let min_y = f32::from(visible_bounds.origin.y);
    let max_x = min_x + vw - f32::from(window_size.width);
    let max_y = min_y + vh - f32::from(window_size.height);

    origin_x = origin_x.clamp(min_x, max_x.max(min_x));
    origin_y = origin_y.clamp(min_y, max_y.max(min_y));

    Bounds {
        origin: point(origin_x.into(), origin_y.into()),
        size: window_size,
    }
}

pub fn contains(bounds: Bounds<Pixels>, point: Point<Pixels>) -> bool {
    let min_x = f32::from(bounds.origin.x);
    let min_y = f32::from(bounds.origin.y);
    let max_x = min_x + f32::from(bounds.size.width);
    let max_y = min_y + f32::from(bounds.size.height);

    let px = f32::from(point.x);
    let py = f32::from(point.y);
    px >= min_x && px <= max_x && py >= min_y && py <= max_y
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size};

    #[test]
    fn normalized_center_round_trips_for_centered_window() {
        let visible = Bounds {
            origin: point(px(100.0), px(200.0)),
            size: size(px(1000.0), px(600.0)),
        };
        let window_size = size(px(200.0), px(120.0));

        let restored =
            bounds_from_normalized(visible, window_size, NormalizedPosition { x: 0.5, y: 0.5 });

        let normalized =
            normalize_center(restored, visible).expect("normalized position should be computable");

        assert!((normalized.x - 0.5).abs() < 0.01);
        assert!((normalized.y - 0.5).abs() < 0.01);
    }
}
