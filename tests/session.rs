//! Integration tests for the persistence + placement pipeline.
//!
//! These walk whole scenarios — quit and relaunch, unplug a monitor, drag a
//! note, corrupt the file — through the real [`Store`] and the real geometry
//! resolver. No GTK, no display, no D-Bus: they run anywhere `cargo test` runs.

use stickies::model::geometry::{cascade, resolve, Monitor};
use stickies::model::note::{Note, NoteGeometry, Palette};
use stickies::model::store::{LoadOutcome, Store};

fn ultrawide() -> Monitor {
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

/// Record a note's geometry the way the app does when the compositor reports
/// where a window ended up after the user dragged it.
fn record_drag(store: &mut Store, id: &str, connector: &str, x: i32, y: i32, w: i32, h: i32) {
    let note = store.get_mut(id).expect("note exists");
    note.geometry = NoteGeometry {
        monitor: Some(connector.into()),
        x,
        y,
        width: w,
        height: h,
    };
}

#[test]
fn notes_come_back_where_they_were_left() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.json");
    let monitors = [ultrawide()];

    // --- session one: create three notes and drag them somewhere ---
    let ids: Vec<String> = {
        let mut store = Store::new(&path);
        let mut ids = Vec::new();
        for (body, palette) in [
            ("Standup at 9:30", Palette::Yellow),
            ("Renew passport", Palette::Blue),
            ("Milk, bread, coffee", Palette::Green),
        ] {
            let mut note = Note::new(palette);
            note.body = body.into();
            ids.push(note.id.clone());
            store.upsert(note);
        }

        record_drag(&mut store, &ids[0], "DP-1", 120, 90, 300, 320);
        record_drag(&mut store, &ids[1], "DP-1", 2400, 400, 360, 280);
        record_drag(&mut store, &ids[2], "DP-1", 4600, 1000, 300, 320);

        store.save().expect("save");
        ids
    };

    // --- session two: relaunch ---
    let (store, outcome) = Store::open(&path);
    assert_eq!(outcome, LoadOutcome::Loaded);
    assert_eq!(store.len(), 3);

    let expected = [
        (120, 90, 300, 320),
        (2400, 400, 360, 280),
        (4600, 1000, 300, 320),
    ];
    for (id, (x, y, w, h)) in ids.iter().zip(expected) {
        let note = store.get(id).expect("note survived");
        let placement = resolve(&note.geometry, &monitors).expect("resolves");
        assert_eq!(
            (placement.x, placement.y, placement.width, placement.height),
            (x, y, w, h),
            "note {id} moved between sessions"
        );
        assert_eq!(placement.connector, "DP-1");
    }

    assert_eq!(store.get(&ids[0]).unwrap().body, "Standup at 9:30");
    assert_eq!(store.get(&ids[1]).unwrap().palette, Palette::Blue);
}

#[test]
fn notes_migrate_when_their_monitor_is_unplugged_and_return_when_it_is_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.json");

    let mut note = Note::new(Palette::Pink);
    note.body = "Far right of the ultrawide".into();
    let id = note.id.clone();
    note.geometry = NoteGeometry {
        monitor: Some("DP-1".into()),
        x: 4600,
        y: 1000,
        width: 300,
        height: 320,
    };

    let mut store = Store::new(&path);
    store.upsert(note);
    store.save().unwrap();

    // Ultrawide unplugged; only the laptop panel is left. The note has to end
    // up somewhere visible on it.
    let docked_off = resolve(&store.get(&id).unwrap().geometry, &[laptop()]).expect("resolves");
    assert_eq!(docked_off.connector, "eDP-1");
    assert!(docked_off.x + docked_off.width <= 1920);
    assert!(docked_off.y + docked_off.height <= 1043);

    // Crucially, the *stored* geometry is untouched by resolution. Plug the
    // ultrawide back in and the note returns to its original spot.
    let (reloaded, _) = Store::open(&path);
    let back = resolve(
        &reloaded.get(&id).unwrap().geometry,
        &[ultrawide(), laptop()],
    )
    .expect("resolves");
    assert_eq!(back.connector, "DP-1");
    assert_eq!((back.x, back.y), (4600, 1000));
}

#[test]
fn a_note_follows_its_monitor_when_the_layout_is_rearranged() {
    // The user moves the ultrawide to sit right of the laptop in the display
    // arrangement. Nothing about the note changes, but its absolute position
    // must track the monitor.
    let mut note = Note::new(Palette::Yellow);
    note.geometry = NoteGeometry {
        monitor: Some("DP-1".into()),
        x: 800,
        y: 200,
        width: 300,
        height: 320,
    };

    let before = ultrawide();
    let mut after = ultrawide();
    after.x = 1920;
    after.y = 0;

    let a = resolve(&note.geometry, &[before.clone(), laptop()]).unwrap();
    let b = resolve(&note.geometry, &[after.clone(), laptop()]).unwrap();

    assert_eq!(
        (a.x, a.y),
        (b.x, b.y),
        "monitor-relative position is stable"
    );
    assert_eq!(before.to_absolute(a.x, a.y), (872, 237));
    assert_eq!(after.to_absolute(b.x, b.y), (2720, 200));
}

#[test]
fn notes_on_a_second_monitor_stay_on_it() {
    let monitors = [ultrawide(), laptop()];

    let mut on_laptop = Note::new(Palette::Purple);
    on_laptop.geometry = NoteGeometry {
        monitor: Some("eDP-1".into()),
        x: 100,
        y: 100,
        width: 300,
        height: 320,
    };

    let placement = resolve(&on_laptop.geometry, &monitors).unwrap();
    assert_eq!(placement.connector, "eDP-1");
    assert_eq!(
        laptop().to_absolute(placement.x, placement.y),
        (100, 1540),
        "absolute position lands on the laptop panel, not the ultrawide"
    );
}

#[test]
fn new_notes_cascade_instead_of_stacking() {
    let monitor = ultrawide();
    let mut store = Store::new("/nonexistent/notes.json");
    let mut occupied: Vec<(i32, i32)> = Vec::new();

    for _ in 0..5 {
        let mut note = Note::new(Palette::Yellow);
        let (x, y) = cascade(
            &monitor,
            &occupied,
            note.geometry.width,
            note.geometry.height,
        );
        note.geometry.monitor = Some(monitor.connector.clone());
        note.geometry.x = x;
        note.geometry.y = y;
        occupied.push((x, y));
        store.upsert(note);
    }

    let positions: Vec<(i32, i32)> = store
        .notes()
        .iter()
        .map(|n| (n.geometry.x, n.geometry.y))
        .collect();
    let unique: std::collections::HashSet<_> = positions.iter().collect();
    assert_eq!(unique.len(), 5, "every new note got its own slot");

    for note in store.notes() {
        let placement = resolve(&note.geometry, std::slice::from_ref(&monitor)).unwrap();
        assert_eq!(
            (placement.x, placement.y),
            (note.geometry.x, note.geometry.y)
        );
    }
}

#[test]
fn hidden_notes_persist_and_can_be_brought_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.json");

    let mut store = Store::new(&path);
    let mut kept = Note::new(Palette::Yellow);
    kept.body = "on screen".into();
    let kept_id = kept.id.clone();

    let mut put_away = Note::new(Palette::Gray);
    put_away.body = "dismissed".into();
    put_away.visible = false;
    let away_id = put_away.id.clone();

    store.upsert(kept);
    store.upsert(put_away);
    store.save().unwrap();

    let (reloaded, _) = Store::open(&path);
    let to_open: Vec<&str> = reloaded
        .notes()
        .iter()
        .filter(|n| n.visible)
        .map(|n| n.id.as_str())
        .collect();
    assert_eq!(to_open, vec![kept_id.as_str()], "only visible notes reopen");

    // "Show All Notes" brings the dismissed one back with its content intact.
    assert_eq!(reloaded.get(&away_id).unwrap().body, "dismissed");
    assert_eq!(reloaded.len(), 2);
}

#[test]
fn deleting_removes_a_note_from_disk_for_good() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.json");

    let mut store = Store::new(&path);
    let doomed = Note::new(Palette::Yellow);
    let doomed_id = doomed.id.clone();
    let survivor = Note::new(Palette::Blue);
    let survivor_id = survivor.id.clone();
    store.upsert(doomed);
    store.upsert(survivor);
    store.save().unwrap();

    store.remove(&doomed_id);
    store.save().unwrap();

    let (reloaded, _) = Store::open(&path);
    assert_eq!(reloaded.len(), 1);
    assert!(reloaded.get(&doomed_id).is_none());
    assert!(reloaded.get(&survivor_id).is_some());
}

#[test]
fn a_truncated_write_never_costs_more_than_the_last_save() {
    // Simulates a crash between saves: the file on disk is the previous good
    // one, and a later corruption is quarantined rather than losing the app.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.json");

    let mut store = Store::new(&path);
    let mut note = Note::new(Palette::Yellow);
    note.body = "important".into();
    let id = note.id.clone();
    store.upsert(note);
    store.save().unwrap();

    // Something outside the app mangles the file.
    let good = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, &good[..good.len() / 2]).unwrap();

    let (recovered, outcome) = Store::open(&path);
    let LoadOutcome::Recovered { backup, .. } = outcome else {
        panic!("expected quarantine");
    };
    assert!(
        recovered.is_empty(),
        "the app starts, empty rather than stuck"
    );
    assert!(!recovered.is_read_only(), "and can save again");

    // The damaged bytes are still there to be rescued by hand.
    let quarantined = std::fs::read_to_string(&backup).unwrap();
    assert!(quarantined.contains(&id) || quarantined.contains("important"));
}

#[test]
fn a_note_edited_in_one_session_keeps_its_geometry_in_the_next() {
    // Guards the split between "the store owns the data" and "the compositor
    // owns the position": editing text must not reset where the note sits.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.json");

    let mut store = Store::new(&path);
    let mut note = Note::new(Palette::Yellow);
    let id = note.id.clone();
    note.geometry = NoteGeometry {
        monitor: Some("DP-1".into()),
        x: 3000,
        y: 700,
        width: 420,
        height: 380,
    };
    store.upsert(note);
    store.save().unwrap();

    let (mut store, _) = Store::open(&path);
    let note = store.get_mut(&id).unwrap();
    note.body = "edited later".into();
    note.touch();
    store.save().unwrap();

    let (reloaded, _) = Store::open(&path);
    let note = reloaded.get(&id).unwrap();
    assert_eq!(note.body, "edited later");
    assert_eq!(note.geometry.x, 3000);
    assert_eq!(note.geometry.y, 700);
    assert_eq!(note.geometry.width, 420);
    assert_eq!(note.geometry.monitor.as_deref(), Some("DP-1"));
}

#[test]
fn every_note_resolves_somewhere_visible_whatever_the_monitor_set() {
    // Property-style sweep: no combination of stored geometry and attached
    // monitors may put a note outside a work area.
    let layouts: [Vec<Monitor>; 3] = [
        vec![ultrawide()],
        vec![laptop()],
        vec![ultrawide(), laptop()],
    ];
    let stored = [
        ("DP-1", -9999, -9999, 300, 320),
        ("DP-1", 9999, 9999, 300, 320),
        ("eDP-1", 0, 0, 4000, 4000),
        ("HDMI-3", 500, 500, 300, 320), // monitor never attached
        ("DP-1", 0, 0, 1, 1),
    ];

    for monitors in &layouts {
        for (connector, x, y, width, height) in stored {
            let geometry = NoteGeometry {
                monitor: Some(connector.into()),
                x,
                y,
                width,
                height,
            };
            let placement = resolve(&geometry, monitors).expect("always resolves");
            let monitor = monitors
                .iter()
                .find(|m| m.connector == placement.connector)
                .expect("resolved onto an attached monitor");

            assert!(placement.x >= 0 && placement.y >= 0, "{placement:?}");
            assert!(
                placement.x + placement.width <= monitor.width,
                "{placement:?} runs off {}",
                monitor.connector
            );
            assert!(
                placement.y + placement.height <= monitor.height,
                "{placement:?} runs off {}",
                monitor.connector
            );
            assert!(placement.width > 0 && placement.height > 0);
        }
    }
}
