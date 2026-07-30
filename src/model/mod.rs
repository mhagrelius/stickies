//! Display-free core: note data, the on-disk store, and monitor geometry.
//!
//! Nothing in here links against GTK, so `cargo test` exercises it with no X
//! server, no Wayland socket and no `gtk::init()`.

pub mod geometry;
pub mod markdown;
pub mod note;
pub mod store;

pub use geometry::{Monitor, Placement};
pub use note::{Note, NoteGeometry, Palette};
pub use store::{Store, StoreError};
