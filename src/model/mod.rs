//! Display-free core: note data, the on-disk store, and monitor geometry.
//!
//! Nothing in here links against GTK, so `cargo test` exercises it with no X
//! server, no Wayland socket and no `gtk::init()`.

pub mod geometry;
// The scanner is a crate now, shared with Brain and Familiar. Re-exported under
// the path it has always had here.
pub use quill as markdown;
pub mod note;
pub mod store;

pub use geometry::{Monitor, Placement};
pub use note::{Note, NoteGeometry, Palette};
pub use store::{Store, StoreError};
