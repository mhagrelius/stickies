//! Multi-monitor placement arithmetic.
//!
//! Notes remember a monitor connector plus monitor-relative coordinates. This
//! module turns that back into a concrete placement against whatever monitors
//! happen to be attached right now, coping with the four things that go wrong
//! in practice: the monitor is gone, the monitor shrank, the layout moved, or
//! the note has never been placed at all.

use super::note::{NoteGeometry, MIN_HEIGHT, MIN_WIDTH};

/// A monitor as the shell extension reports it.
///
/// The rectangle is the monitor's **work area** in absolute compositor
/// coordinates — panels and docks already subtracted — so a note clamped inside
/// it never hides under the GNOME top bar or the Ubuntu dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// Connector name, e.g. `"DP-1"`. Stable across reboots for a given port.
    pub connector: String,
    /// Human-readable name, e.g. `"Dell U4924DW"`. Display only.
    pub display_name: String,
    pub primary: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Monitor {
    /// Translate a monitor-relative point into absolute compositor coordinates.
    pub fn to_absolute(&self, x: i32, y: i32) -> (i32, i32) {
        (self.x + x, self.y + y)
    }

    /// Translate an absolute point into monitor-relative coordinates.
    pub fn to_relative(&self, x: i32, y: i32) -> (i32, i32) {
        (x - self.x, y - self.y)
    }

    /// Does this absolute point fall inside the work area?
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// A resolved placement: which monitor, and monitor-relative position and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub connector: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Choose the monitor a note should appear on.
///
/// Preference order: the remembered connector, then the primary monitor, then
/// whatever comes first. Returns `None` only when no monitors are attached,
/// which the caller treats as "the shell isn't answering, skip placement".
pub fn pick_monitor<'a>(connector: Option<&str>, monitors: &'a [Monitor]) -> Option<&'a Monitor> {
    if let Some(connector) = connector {
        if let Some(found) = monitors.iter().find(|m| m.connector == connector) {
            return Some(found);
        }
    }
    monitors
        .iter()
        .find(|m| m.primary)
        .or_else(|| monitors.first())
}

/// Resolve stored geometry into a placement valid for the current monitor set.
///
/// The result is always fully inside the target monitor's work area: size is
/// capped at the work area, then the position is clamped so no edge hangs off.
/// A note whose monitor was unplugged reappears on the primary monitor at the
/// same relative spot, clamped if the new monitor is smaller.
pub fn resolve(geometry: &NoteGeometry, monitors: &[Monitor]) -> Option<Placement> {
    let geometry = geometry.normalized();
    let monitor = pick_monitor(geometry.monitor.as_deref(), monitors)?;

    // Cap the size first: clamping a position against a size that does not fit
    // would produce a negative range.
    let width = geometry
        .width
        .clamp(MIN_WIDTH.min(monitor.width), monitor.width);
    let height = geometry
        .height
        .clamp(MIN_HEIGHT.min(monitor.height), monitor.height);

    Some(Placement {
        connector: monitor.connector.clone(),
        x: geometry.x.clamp(0, monitor.width - width),
        y: geometry.y.clamp(0, monitor.height - height),
        width,
        height,
    })
}

/// Distance between successive cascaded notes, in logical pixels.
const CASCADE_STEP: i32 = 34;
/// Two notes are considered stacked if both axes are within this many pixels.
const CASCADE_TOLERANCE: i32 = 16;

/// Pick an opening position for a note that has never been placed.
///
/// Notes cascade down and right from the top-left of the work area, skipping
/// slots already occupied, so creating several notes in a row does not bury
/// them in one pile. When every slot in the cascade is taken it wraps back to
/// the start — at that point the user has enough notes on screen that exact
/// placement stopped mattering.
pub fn cascade(monitor: &Monitor, occupied: &[(i32, i32)], width: i32, height: i32) -> (i32, i32) {
    let width = width.min(monitor.width);
    let height = height.min(monitor.height);
    let max_x = (monitor.width - width).max(0);
    let max_y = (monitor.height - height).max(0);

    // Inset from the corner so the first note is not jammed against the edge.
    let base_x = (monitor.width / 20).min(max_x);
    let base_y = (monitor.height / 20).min(max_y);

    let slots = slot_count(max_x - base_x, max_y - base_y);
    for step in 0..slots {
        let candidate = (
            (base_x + step * CASCADE_STEP).min(max_x),
            (base_y + step * CASCADE_STEP).min(max_y),
        );
        let taken = occupied.iter().any(|&(x, y)| {
            (x - candidate.0).abs() <= CASCADE_TOLERANCE
                && (y - candidate.1).abs() <= CASCADE_TOLERANCE
        });
        if !taken {
            return candidate;
        }
    }
    (base_x, base_y)
}

/// How many distinct cascade slots fit before running off the work area.
fn slot_count(span_x: i32, span_y: i32) -> i32 {
    let steps = span_x.min(span_y) / CASCADE_STEP;
    steps.clamp(1, 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ultrawide() -> Monitor {
        // The machine this was built on: one 5120x1440 ultrawide, minus the
        // GNOME top bar and the Ubuntu dock on the left.
        Monitor {
            connector: "DP-1".into(),
            display_name: "Dell U4924DW".into(),
            primary: true,
            x: 72,
            y: 37,
            width: 5048,
            height: 1403,
        }
    }

    fn laptop() -> Monitor {
        Monitor {
            connector: "eDP-1".into(),
            display_name: "Built-in display".into(),
            primary: false,
            x: 0,
            y: 1440,
            width: 1920,
            height: 1043,
        }
    }

    fn geom(monitor: Option<&str>, x: i32, y: i32, w: i32, h: i32) -> NoteGeometry {
        NoteGeometry {
            monitor: monitor.map(str::to_string),
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn coordinate_translation_round_trips() {
        let m = ultrawide();
        let absolute = m.to_absolute(100, 200);
        assert_eq!(absolute, (172, 237));
        assert_eq!(m.to_relative(absolute.0, absolute.1), (100, 200));
    }

    #[test]
    fn contains_respects_the_work_area_origin() {
        let m = ultrawide();
        assert!(m.contains(72, 37));
        assert!(m.contains(5119, 1439));
        assert!(!m.contains(71, 37), "left of the dock is off the work area");
        assert!(
            !m.contains(72, 36),
            "under the top bar is off the work area"
        );
        assert!(!m.contains(5120, 1439));
    }

    #[test]
    fn picks_the_remembered_monitor() {
        let monitors = [ultrawide(), laptop()];
        let picked = pick_monitor(Some("eDP-1"), &monitors).unwrap();
        assert_eq!(picked.connector, "eDP-1");
    }

    #[test]
    fn falls_back_to_primary_when_the_monitor_is_unplugged() {
        let monitors = [ultrawide(), laptop()];
        let picked = pick_monitor(Some("HDMI-2"), &monitors).unwrap();
        assert_eq!(picked.connector, "DP-1", "primary monitor takes over");
    }

    #[test]
    fn falls_back_to_the_first_monitor_when_none_is_primary() {
        let mut monitors = [ultrawide(), laptop()];
        monitors[0].primary = false;
        let picked = pick_monitor(None, &monitors).unwrap();
        assert_eq!(picked.connector, "DP-1");
    }

    #[test]
    fn no_monitors_means_no_placement() {
        assert!(pick_monitor(Some("DP-1"), &[]).is_none());
        assert!(resolve(&geom(Some("DP-1"), 0, 0, 300, 320), &[]).is_none());
    }

    #[test]
    fn a_note_that_already_fits_is_left_alone() {
        let monitors = [ultrawide()];
        let placement = resolve(&geom(Some("DP-1"), 4000, 900, 300, 320), &monitors).unwrap();
        assert_eq!(
            placement,
            Placement {
                connector: "DP-1".into(),
                x: 4000,
                y: 900,
                width: 300,
                height: 320,
            }
        );
    }

    #[test]
    fn a_note_hanging_off_the_right_edge_is_pulled_back_on() {
        let monitors = [ultrawide()];
        let placement = resolve(&geom(Some("DP-1"), 5000, 1390, 300, 320), &monitors).unwrap();
        assert_eq!(placement.x, 5048 - 300);
        assert_eq!(placement.y, 1403 - 320);
    }

    #[test]
    fn negative_coordinates_are_pulled_back_on() {
        let monitors = [ultrawide()];
        let placement = resolve(&geom(Some("DP-1"), -500, -40, 300, 320), &monitors).unwrap();
        assert_eq!((placement.x, placement.y), (0, 0));
    }

    #[test]
    fn a_note_from_a_wide_monitor_is_clamped_onto_a_narrow_one() {
        // Note lived at x=4000 on the ultrawide; that monitor is now gone and
        // only the 1920px laptop panel remains.
        let monitors = [laptop()];
        let placement = resolve(&geom(Some("DP-1"), 4000, 900, 300, 320), &monitors).unwrap();
        assert_eq!(placement.connector, "eDP-1");
        assert_eq!(placement.x, 1920 - 300);
        assert_eq!(placement.y, 1043 - 320);
    }

    #[test]
    fn an_oversized_note_is_capped_to_the_work_area() {
        let monitors = [laptop()];
        let placement = resolve(&geom(Some("eDP-1"), 0, 0, 9000, 9000), &monitors).unwrap();
        assert_eq!((placement.width, placement.height), (1920, 1043));
        assert_eq!((placement.x, placement.y), (0, 0));
    }

    #[test]
    fn an_undersized_note_is_grown_to_the_minimum() {
        let monitors = [ultrawide()];
        let placement = resolve(&geom(Some("DP-1"), 10, 10, 1, 1), &monitors).unwrap();
        assert_eq!((placement.width, placement.height), (MIN_WIDTH, MIN_HEIGHT));
    }

    #[test]
    fn a_monitor_smaller_than_the_minimum_note_still_resolves() {
        // Degenerate but must not panic or produce an inverted clamp range.
        let tiny = Monitor {
            connector: "VIRTUAL-1".into(),
            display_name: "Tiny".into(),
            primary: true,
            x: 0,
            y: 0,
            width: 100,
            height: 90,
        };
        let placement = resolve(&geom(None, 0, 0, 300, 320), &[tiny]).unwrap();
        assert_eq!((placement.width, placement.height), (100, 90));
        assert_eq!((placement.x, placement.y), (0, 0));
    }

    #[test]
    fn resolution_is_idempotent() {
        let monitors = [ultrawide()];
        let first = resolve(&geom(Some("DP-1"), 9999, 9999, 300, 320), &monitors).unwrap();
        let second = resolve(
            &geom(
                Some(&first.connector),
                first.x,
                first.y,
                first.width,
                first.height,
            ),
            &monitors,
        )
        .unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn relative_coordinates_survive_a_layout_shift() {
        // Same physical monitor, moved in the layout (a second screen was added
        // to its left). A note at relative (400, 300) must stay visually put.
        let before = ultrawide();
        let mut after = ultrawide();
        after.x += 1920;

        let g = geom(Some("DP-1"), 400, 300, 300, 320);
        let a = resolve(&g, std::slice::from_ref(&before)).unwrap();
        let b = resolve(&g, std::slice::from_ref(&after)).unwrap();
        assert_eq!((a.x, a.y), (b.x, b.y), "relative position is unchanged");
        assert_eq!(
            before.to_absolute(a.x, a.y).0 + 1920,
            after.to_absolute(b.x, b.y).0,
            "absolute position tracks the monitor"
        );
    }

    #[test]
    fn cascade_offsets_successive_notes() {
        let m = ultrawide();
        let first = cascade(&m, &[], 300, 320);
        let second = cascade(&m, &[first], 300, 320);
        let third = cascade(&m, &[first, second], 300, 320);

        assert_ne!(first, second);
        assert_ne!(second, third);
        assert_eq!(second.0 - first.0, CASCADE_STEP);
        assert_eq!(second.1 - first.1, CASCADE_STEP);
    }

    #[test]
    fn cascade_reuses_a_freed_slot() {
        let m = ultrawide();
        let first = cascade(&m, &[], 300, 320);
        let second = cascade(&m, &[first], 300, 320);
        // Close the first note; the next one should reclaim its slot.
        assert_eq!(cascade(&m, &[second], 300, 320), first);
    }

    #[test]
    fn cascade_always_lands_inside_the_work_area() {
        let m = ultrawide();
        let mut occupied = Vec::new();
        for _ in 0..64 {
            let slot = cascade(&m, &occupied, 300, 320);
            assert!(slot.0 >= 0 && slot.0 + 300 <= m.width, "x={}", slot.0);
            assert!(slot.1 >= 0 && slot.1 + 320 <= m.height, "y={}", slot.1);
            occupied.push(slot);
        }
    }

    #[test]
    fn cascade_survives_a_monitor_smaller_than_the_note() {
        let tiny = Monitor {
            connector: "VIRTUAL-1".into(),
            display_name: "Tiny".into(),
            primary: true,
            x: 0,
            y: 0,
            width: 100,
            height: 90,
        };
        assert_eq!(cascade(&tiny, &[], 300, 320), (0, 0));
    }
}
