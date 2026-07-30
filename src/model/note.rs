//! The note itself: content, colour, and remembered geometry.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default size of a freshly created note, in logical pixels.
pub const DEFAULT_WIDTH: i32 = 300;
pub const DEFAULT_HEIGHT: i32 = 320;

/// A note may not be shrunk below this; smaller and the text view is unusable.
pub const MIN_WIDTH: i32 = 180;
pub const MIN_HEIGHT: i32 = 140;

/// Where a note wants to sit.
///
/// `x`/`y` are **relative to the origin of the monitor's work area**, not to the
/// global compositor coordinate space. That is what makes the app multi-monitor
/// aware in a useful way: unplugging a monitor, changing its resolution, or
/// rearranging the layout moves the whole work area, and monitor-relative
/// coordinates follow it instead of stranding notes in dead space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteGeometry {
    /// Connector name of the monitor the note was last on (`"DP-1"`, `"eDP-1"`).
    /// `None` for a note that has never been placed.
    #[serde(default)]
    pub monitor: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Default for NoteGeometry {
    fn default() -> Self {
        Self {
            monitor: None,
            x: 0,
            y: 0,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
        }
    }
}

impl NoteGeometry {
    /// Clamp the stored size into the range a note window can actually take.
    /// Sizes arrive from disk and from the compositor, so neither is trusted.
    pub fn normalized(&self) -> Self {
        Self {
            monitor: self.monitor.clone(),
            x: self.x,
            y: self.y,
            width: self.width.max(MIN_WIDTH),
            height: self.height.max(MIN_HEIGHT),
        }
    }
}

/// The colour of a note. A fixed palette rather than a free colour picker: the
/// point of sticky-note colour is fast visual grouping, and seven distinguishable
/// options do that better than sixteen million.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Palette {
    #[default]
    Yellow,
    Green,
    Blue,
    Purple,
    Pink,
    Orange,
    Gray,
}

impl Palette {
    /// Every palette entry, in the order they appear in the colour menu.
    pub const ALL: [Palette; 7] = [
        Palette::Yellow,
        Palette::Green,
        Palette::Blue,
        Palette::Purple,
        Palette::Pink,
        Palette::Orange,
        Palette::Gray,
    ];

    /// Stable machine name — used as the CSS class suffix, the action target,
    /// and the serialised form.
    pub const fn id(self) -> &'static str {
        match self {
            Palette::Yellow => "yellow",
            Palette::Green => "green",
            Palette::Blue => "blue",
            Palette::Purple => "purple",
            Palette::Pink => "pink",
            Palette::Orange => "orange",
            Palette::Gray => "gray",
        }
    }

    /// Translatable display name for the colour menu.
    pub const fn label(self) -> &'static str {
        match self {
            Palette::Yellow => "Yellow",
            Palette::Green => "Green",
            Palette::Blue => "Blue",
            Palette::Purple => "Purple",
            Palette::Pink => "Pink",
            Palette::Orange => "Orange",
            Palette::Gray => "Grey",
        }
    }

    /// The CSS class applied to the note window for this colour.
    pub fn css_class(self) -> String {
        format!("note-{}", self.id())
    }

    pub fn from_id(id: &str) -> Option<Palette> {
        Palette::ALL.into_iter().find(|p| p.id() == id)
    }
}

/// One sticky note, exactly as it is persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub palette: Palette,
    #[serde(default)]
    pub geometry: NoteGeometry,
    /// Whether the note's window was open when the app last exited. Closing a
    /// note hides it; it stays in the store and comes back with "Show All".
    #[serde(default = "default_true")]
    pub visible: bool,
    /// Keep the note above other windows.
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub modified: i64,
}

fn default_true() -> bool {
    true
}

impl Note {
    /// A new, empty, visible note with a generated ID.
    pub fn new(palette: Palette) -> Self {
        let now = now_secs();
        Self {
            id: generate_id(),
            body: String::new(),
            palette,
            geometry: NoteGeometry::default(),
            visible: true,
            pinned: false,
            created: now,
            modified: now,
        }
    }

    /// One-line summary for window titles, the notes list and screen readers.
    ///
    /// Markdown syntax is stripped: a title bar reading "# Heading" advertises
    /// the file format rather than the note. Falls back to a placeholder so a
    /// blank note is still identifiable.
    pub fn title(&self) -> String {
        const MAX: usize = 40;
        let plain = crate::model::markdown::strip(&self.body);
        let first = plain
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("");

        if first.is_empty() {
            return "Empty Note".to_string();
        }
        if first.chars().count() <= MAX {
            return first.to_string();
        }
        let truncated: String = first.chars().take(MAX).collect();
        // Prefer breaking at the last word boundary so the ellipsis reads well.
        match truncated.rfind(' ') {
            Some(idx) if idx >= MAX / 2 => format!("{}…", &truncated[..idx]),
            _ => format!("{truncated}…"),
        }
    }

    pub fn touch(&mut self) {
        self.modified = now_secs();
    }
}

/// The colour a new note should take, given the notes that already exist.
///
/// Picks the least-used palette, so a fresh colour is chosen while any remain
/// unused and the spread stays even afterwards. Colour is only worth having if
/// it distinguishes notes; seven identical yellow ones tell you nothing.
///
/// Ties break toward [`Palette::ALL`] order, which makes the sequence
/// deterministic — the same set of notes always yields the same next colour.
pub fn least_used_palette(notes: &[Note]) -> Palette {
    Palette::ALL
        .into_iter()
        .min_by_key(|palette| notes.iter().filter(|n| n.palette == *palette).count())
        .unwrap_or_default()
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Generate an ID unique within this store.
///
/// Notes are only ever created by the single running instance (GtkApplication
/// enforces uniqueness), so a monotonic counter combined with the creation
/// timestamp is sufficient — no need to pull in a UUID dependency.
fn generate_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{seq:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_unique() {
        let ids: std::collections::HashSet<_> = (0..1000).map(|_| generate_id()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn title_strips_markdown_syntax() {
        // Reported: a note starting "# Heading Goes Here" put the hash in the
        // title bar, advertising the file format rather than the note.
        let mut note = Note::new(Palette::Yellow);
        note.body = "# Heading Goes Here\n\nbody text".into();
        assert_eq!(note.title(), "Heading Goes Here");

        note.body = "- **oat** milk".into();
        assert_eq!(note.title(), "oat milk");

        note.body = "> `remember` this".into();
        assert_eq!(note.title(), "remember this");
    }

    #[test]
    fn title_still_truncates_after_stripping() {
        // The limit applies to what is shown, not to the raw markup, or a
        // heavily formatted line would be cut far too short.
        let mut note = Note::new(Palette::Yellow);
        // No trailing space inside the emphasis: "** ... **" is deliberately
        // not bold, so it would not be stripped.
        note.body = format!("# **{}**", "word ".repeat(20).trim_end());
        let title = note.title();
        assert!(title.chars().count() <= 41, "{title:?}");
        assert!(!title.starts_with('#') && !title.starts_with('*'));
    }

    #[test]
    fn title_uses_first_non_empty_line() {
        let mut note = Note::new(Palette::Yellow);
        note.body = "\n\n  Buy oat milk  \nand bread".into();
        assert_eq!(note.title(), "Buy oat milk");
    }

    #[test]
    fn title_of_blank_note_is_a_placeholder() {
        let note = Note::new(Palette::Yellow);
        assert_eq!(note.title(), "Empty Note");

        let mut whitespace = Note::new(Palette::Yellow);
        whitespace.body = "   \n\t\n".into();
        assert_eq!(whitespace.title(), "Empty Note");
    }

    #[test]
    fn title_truncates_on_a_word_boundary() {
        let mut note = Note::new(Palette::Yellow);
        note.body = "Remember to renew the passport before the trip to Lisbon".into();
        let title = note.title();
        assert!(title.ends_with('…'));
        assert!(title.chars().count() <= 41);
        // Broke at a space, so no partial word survives before the ellipsis.
        assert!(!title.trim_end_matches('…').ends_with(' '));
        assert!(note.body.starts_with(title.trim_end_matches('…')));
    }

    #[test]
    fn title_truncates_mid_word_when_there_is_no_space() {
        let mut note = Note::new(Palette::Yellow);
        note.body = "a".repeat(80);
        assert_eq!(note.title().chars().count(), 41);
    }

    #[test]
    fn title_handles_multibyte_content() {
        let mut note = Note::new(Palette::Yellow);
        note.body = "🎉".repeat(60);
        let title = note.title();
        assert!(title.ends_with('…'));
        assert_eq!(title.chars().count(), 41);
    }

    fn with_palettes(palettes: &[Palette]) -> Vec<Note> {
        palettes.iter().map(|p| Note::new(*p)).collect()
    }

    #[test]
    fn the_first_note_is_the_first_palette() {
        assert_eq!(least_used_palette(&[]), Palette::ALL[0]);
    }

    #[test]
    fn new_notes_take_a_colour_not_yet_in_use() {
        // Walk the whole palette: each new note must introduce a new colour
        // while any remain unused.
        let mut notes: Vec<Note> = Vec::new();
        let mut seen = Vec::new();
        for _ in 0..Palette::ALL.len() {
            let palette = least_used_palette(&notes);
            assert!(!seen.contains(&palette), "{palette:?} was reused too early");
            seen.push(palette);
            notes.push(Note::new(palette));
        }
        assert_eq!(seen.len(), Palette::ALL.len(), "every colour got used once");
    }

    #[test]
    fn it_skips_colours_already_taken() {
        let notes = with_palettes(&[Palette::Yellow, Palette::Green, Palette::Blue]);
        let chosen = least_used_palette(&notes);
        assert!(
            !matches!(chosen, Palette::Yellow | Palette::Green | Palette::Blue),
            "picked {chosen:?}, which is already on screen"
        );
        assert_eq!(chosen, Palette::Purple, "and it follows the palette order");
    }

    #[test]
    fn once_every_colour_is_used_it_evens_out() {
        // One of each, plus extra yellows: the next note must not be yellow.
        let mut palettes: Vec<Palette> = Palette::ALL.to_vec();
        palettes.extend([Palette::Yellow, Palette::Yellow]);
        let notes = with_palettes(&palettes);

        let chosen = least_used_palette(&notes);
        assert_ne!(chosen, Palette::Yellow, "the most-used colour must not win");
        let used = notes.iter().filter(|n| n.palette == chosen).count();
        assert_eq!(used, 1, "it picked one of the least-used colours");
    }

    #[test]
    fn deleting_a_note_frees_its_colour_again() {
        let mut notes = with_palettes(&Palette::ALL[..3]);
        notes.remove(1); // drop the green one
        assert_eq!(least_used_palette(&notes), Palette::Green);
    }

    #[test]
    fn palette_ids_round_trip() {
        for palette in Palette::ALL {
            assert_eq!(Palette::from_id(palette.id()), Some(palette));
        }
        assert_eq!(Palette::from_id("chartreuse"), None);
    }

    #[test]
    fn palette_serialises_as_its_id() {
        let json = serde_json::to_string(&Palette::Purple).unwrap();
        assert_eq!(json, "\"purple\"");
        for palette in Palette::ALL {
            let json = serde_json::to_string(&palette).unwrap();
            assert_eq!(json, format!("\"{}\"", palette.id()));
        }
    }

    #[test]
    fn geometry_normalisation_enforces_minimums() {
        let tiny = NoteGeometry {
            monitor: Some("DP-1".into()),
            x: 10,
            y: 20,
            width: 5,
            height: 5,
        };
        let normalized = tiny.normalized();
        assert_eq!(normalized.width, MIN_WIDTH);
        assert_eq!(normalized.height, MIN_HEIGHT);
        // Position is left alone — clamping into a monitor is geometry's job.
        assert_eq!((normalized.x, normalized.y), (10, 20));
        assert_eq!(normalized.monitor.as_deref(), Some("DP-1"));
    }

    #[test]
    fn note_deserialises_from_a_minimal_record() {
        // Forward compatibility: old or hand-edited files need only an id.
        let note: Note = serde_json::from_str(r#"{"id":"abc"}"#).unwrap();
        assert_eq!(note.id, "abc");
        assert_eq!(note.palette, Palette::Yellow);
        assert!(note.visible, "notes default to visible");
        assert!(!note.pinned);
        assert_eq!(note.geometry, NoteGeometry::default());
    }
}
