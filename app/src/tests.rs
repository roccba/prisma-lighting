//! Unit tests of the shell (pure helpers and the UI fixture).

use super::*;

fn legacy(enabled: bool, ip: &str) -> NetworkConfig {
    NetworkConfig {
        artnet_enabled: enabled,
        artnet_ip: ip.into(),
        artnet_port: 6454,
    }
}

#[test]
fn an_enabled_legacy_target_becomes_one_node_with_the_patch_universes() {
    let mut show = empty_show();
    show.network = ShowNetwork::default();
    prepare_show_network(&mut show, &legacy(true, "2.255.255.255"));
    let net = &show.network;
    assert!(net.initialized && net.artnet_enabled);
    assert_eq!(net.nodes.len(), 1);
    assert_eq!(net.nodes[0].ip, Ipv4Addr::new(2, 255, 255, 255));
    assert_eq!(net.nodes[0].ports, patch_universes(&show.patch));
}

#[test]
fn a_disabled_legacy_stays_on_the_virtual_sink_and_migrates_once() {
    let mut show = empty_show();
    show.network = ShowNetwork::default();
    prepare_show_network(&mut show, &legacy(false, "10.0.0.1"));
    assert!(show.network.initialized && !show.network.artnet_enabled);
    assert!(show.network.nodes.is_empty());
    // Already initialized: a later enabled legacy config must not add a node.
    prepare_show_network(&mut show, &legacy(true, "10.0.0.1"));
    assert!(show.network.nodes.is_empty());
}

#[test]
fn empty_show_default_zone_has_a_seeded_lit_grid() {
    // Regression: the default boot zone must have assignable scene cells and start lit,
    // otherwise mapping pixels into it leaves the effect grid disabled (the reconcile
    // path preserves an existing zone's grid, so an empty one would never fill in).
    let show = empty_show();
    assert_eq!(show.zones.len(), 1, "empty show has one default zone");
    assert_eq!(show.tracks.len(), 1, "the default zone has its own track");
    assert_eq!(
        show.tracks[0].clips.len(),
        GRID_CELLS,
        "default zone's scene grid must be seeded"
    );
    assert_eq!(show.zones[0].live.dimmer, 1.0, "default zone starts lit");
}

#[test]
fn a_new_show_is_a_mapped_50x50_zone_with_an_empty_grid() {
    let show = empty_show();
    let patch = &show.patch;
    assert_eq!(patch.fixtures.len(), (DEFAULT_RIG * DEFAULT_RIG) as usize);
    assert_eq!(patch.zones[0].mapping_size, [DEFAULT_RIG, DEFAULT_RIG, 1]);
    assert_eq!(patch.matrix_size[..2], [DEFAULT_RIG, DEFAULT_RIG]);
    let universes: std::collections::BTreeSet<u16> = patch
        .fixtures
        .iter()
        .flat_map(|f| f.pixels.iter().map(|p| p.universe))
        .collect();
    assert_eq!(universes.len(), 15, "2500 RGB px at 170 per universe");
    assert_eq!(patch.fixtures[0].name, "Generic RGB 1 pixel #1");
    patch.clone().compile().expect("default patch compiles");

    // Every cell empty and nothing running: cells are assigned lazily (first edit / GO).
    let track = show.home_track(patch.zones[0].id).expect("its own track");
    assert_eq!(track.active, None);
    assert_eq!(track.clips.len(), GRID_CELLS);
    assert!(
        track.clips.iter().all(|c| c.layers.is_empty()),
        "no pre-assigned cell"
    );
}

/// The demo show prepared the way `run_show` prepares a show it opens.
fn prepared_demo() -> Show {
    let mut show = prisma_engine::demo::demo_show();
    show.patch.fill_zone_members_from_ownership();
    show.patch.fit_matrix_size();
    pad_grids(&mut show);
    prepare_show_network(&mut show, &NetworkConfig::default());
    show
}

/// What the UI reads from the app at start and while it runs, for the demo show: the
/// Playwright harness answers the mocked IPC from it (LIVE-MODEL-SPEC §10.1). MIDI is
/// an APC40 mkII connected with its default mappings, so learn badges and the bank
/// window show.
fn ui_fixture() -> serde_json::Value {
    let show = prepared_demo();
    let apc = DeviceProfile::apc40_mk2();
    let port = "APC40 mkII:APC40 mkII MIDI 1 28:0".to_string();
    let midi = MidiStatus {
        ports: vec![port.clone()],
        selected: None,
        connected: Some(port.clone()),
        output: Some(port),
        device: apc.display_name.clone(),
        error: None,
        learning: None,
        mappings: apc.default_mappings.clone(),
        palette: apc
            .palette()
            .map(|p| p.iter().map(|c| format!("#{c:06x}")).collect())
            .unwrap_or_default(),
        bank: prisma_engine::Bank::default(),
        device_defaults: apc.default_mappings.len(),
        grid_window: apc.surface.bank.as_ref().map(|_| apc.window()),
    };
    // The panel for every clip with an effect and for "the running clip" (`null`); an
    // empty clip's panel is `empty_panel` with its track and index filled in.
    let mut panels = serde_json::Map::new();
    for t in &show.tracks {
        let assigned = (0..t.clips.len()).filter(|&i| !t.clips[i].layers.is_empty());
        for cell in assigned.map(Some).chain([None]) {
            let key = match cell {
                Some(i) => format!("{}:{i}", t.id),
                None => format!("{}:null", t.id),
            };
            let panel = effect_panel_info(&show, t.id, cell, None, None);
            panels.insert(key, serde_json::to_value(panel).unwrap());
        }
    }
    serde_json::json!({
        "main_zones": zone_infos(&show, |_| None),
        "main_grid": grid_info(&show, |_| None, |_| None),
        "main_master": master_info(&show),
        "main_clock": ClockInfo { bpm: 120.0 },
        "main_effect_catalogue": main_effect_catalogue(),
        "main_effect_panel": panels,
        "empty_panel": effect_panel_info(&show, Uuid::nil(), None, None, None),
        "main_selection": SelectionInfo { track: None, clip: None },
        "main_last_saved": null,
        "main_recovery_info": null,
        "editor_preview_port": 0,
        // The patch editor on the demo rig (the harness opens it with `?window=editor`).
        "editor_get_state": state_of(&PatchDraft::new(show.patch.clone())),
        "editor_fixture_types": FixtureLibrary::seed().types,
        "control_keys": prisma_engine::keys::default_bindings(),
        "midi_status": midi,
    })
}

/// `ui/e2e/fixtures/demo.json` is what [`ui_fixture`] produces. Regenerate with
/// `UPDATE_UI_FIXTURE=1 cargo test ui_fixture` after changing the demo show or a DTO.
#[test]
fn ui_fixture_is_current() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/e2e/fixtures/demo.json");
    let fresh = serde_json::to_string_pretty(&ui_fixture()).unwrap() + "\n";
    if std::env::var_os("UPDATE_UI_FIXTURE").is_some() {
        std::fs::write(&path, &fresh).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == fresh,
        "ui/e2e/fixtures/demo.json is stale: run `UPDATE_UI_FIXTURE=1 cargo test \
         ui_fixture` in app/"
    );
}

#[test]
fn the_demo_show_stays_on_the_virtual_sink_and_keeps_its_clips() {
    // What `run_show` does to it, with an enabled legacy target on the machine.
    let mut show = prisma_engine::demo::demo_show();
    let authored: Vec<usize> = show.tracks.iter().map(|t| t.clips.len()).collect();
    show.patch.fill_zone_members_from_ownership();
    show.patch.fit_matrix_size();
    show.patch.compile().expect("compiles");
    pad_grids(&mut show);
    prepare_show_network(&mut show, &legacy(true, "2.255.255.255"));
    assert!(!show.network.artnet_enabled && show.network.nodes.is_empty());
    for (t, n) in show.tracks.iter().zip(authored) {
        assert_eq!(t.clips.len(), GRID_CELLS.max(n), "{} padded", t.name);
        assert!(!t.clips[0].layers.is_empty(), "{} plays at open", t.name);
    }
}

#[test]
fn pad_grids_fills_short_tracks_and_keeps_authored_clips() {
    // Idempotent: it fills a track with no clips and pads a short one, keeping its
    // authored clips at their index (so a loaded show's real content survives). It
    // never adds tracks: removing a zone's only track must stick.
    let mut show = Show::new("t", empty_patch());
    assert!(show.tracks[0].clips.is_empty());
    let mut authored = Track::new("extra", prisma_engine::Destination::default());
    authored.clips = vec![Cell::empty()];
    authored.clips[0].fade_time = std::time::Duration::from_millis(250);
    show.tracks.push(authored);

    pad_grids(&mut show);
    pad_grids(&mut show);

    assert_eq!(show.tracks.len(), 2, "no track added or removed");
    assert_eq!(show.tracks[0].clips.len(), GRID_CELLS, "empty track seeded");
    assert_eq!(show.tracks[1].clips.len(), GRID_CELLS, "short track padded");
    assert_eq!(
        show.tracks[1].clips[0].fade_time,
        std::time::Duration::from_millis(250),
        "authored clip kept at its index"
    );

    show.tracks.clear();
    pad_grids(&mut show);
    assert!(
        show.tracks.is_empty(),
        "a zone with no track stays without one"
    );
}

#[test]
fn new_tracks_are_named_track_n_with_the_first_free_number() {
    let mut show = Show::new("t", empty_patch());
    assert_eq!(next_track_name(&show), "Track 1");
    show.tracks[0].name = "Track 1".into();
    let mut t = Track::new("Track 3", prisma_engine::Destination::default());
    show.tracks.push(t.clone());
    assert_eq!(next_track_name(&show), "Track 2");
    t.name = "Track 2".into();
    show.tracks.push(t);
    assert_eq!(next_track_name(&show), "Track 4");
}

#[test]
fn a_cell_shows_its_effects_first_colour_group() {
    let mut p = ParamSet::new();
    p.set("r", 0.5);
    p.set("g", 0.25);
    p.set("b", 0.0);
    assert_eq!(effect_main_color("solid", &p), Some([0.5, 0.25, 0.0]));
    assert_eq!(effect_main_color("no-such-effect", &p), None);
}

#[test]
fn a_scene_label_is_made_on_demand_and_an_empty_name_clears_it() {
    let mut show = empty_show();
    assert!(show.scenes.is_empty());
    scene_label(&mut show, 2).name = Some("Fuerte".into());
    assert_eq!(
        show.scenes.len(),
        3,
        "the rows before it get an empty label"
    );
    assert_eq!(show.scenes[2].name.as_deref(), Some("Fuerte"));
    scene_label(&mut show, 0).color = Some([10, 20, 30]);
    assert_eq!(show.scenes.len(), 3);
    assert_eq!(show.scenes[0].color, Some([10, 20, 30]));
}

#[test]
fn tracks_reorder_globally() {
    let za = Uuid::new_v4();
    let zb = Uuid::new_v4();
    let t = |name: &str, z: Uuid| Track::new(name, prisma_engine::Destination::zone(z));
    let mut tracks = vec![t("a1", za), t("a2", za), t("a3", za), t("b1", zb)];
    let ids: Vec<Uuid> = tracks.iter().map(|t| t.id).collect();
    let names = |ts: &[Track]| ts.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    move_track(&mut tracks, ids[2], Some(ids[0]));
    assert_eq!(names(&tracks), ["a3", "a1", "a2", "b1"]);
    move_track(&mut tracks, ids[2], None);
    assert_eq!(
        names(&tracks),
        ["a1", "a2", "b1", "a3"],
        "None = last of the show"
    );
    move_track(&mut tracks, ids[3], Some(ids[0]));
    assert_eq!(names(&tracks), ["b1", "a1", "a2", "a3"], "across zones");
    move_track(&mut tracks, ids[0], Some(ids[0]));
    move_track(&mut tracks, ids[0], Some(Uuid::new_v4()));
    assert_eq!(names(&tracks), ["b1", "a1", "a2", "a3"], "no-ops");
}

#[test]
fn a_destination_is_the_patchs_zones_in_patch_order() {
    let mut patch = empty_patch();
    for name in ["B", "C"] {
        let mut z = patch.zones[0].clone();
        z.id = Uuid::new_v4();
        z.name = name.into();
        patch.zones.push(z);
    }
    let [a, b, c] = [0, 1, 2].map(|i| patch.zones[i].id);
    let zones = |asked: &[Uuid]| destination_of(&patch, asked, &[]).map(|d| d.zones);
    assert_eq!(zones(&[c, a, a]).unwrap(), [a, c]);
    assert_eq!(zones(&[b, Uuid::new_v4()]).unwrap(), [b]);
    assert!(zones(&[]).is_err());
    assert!(zones(&[Uuid::new_v4()]).is_err());

    // Groups: patch order, each once; a group alone is a destination.
    for name in ["G1", "G2"] {
        patch.groups.push(Group {
            id: Uuid::new_v4(),
            name: name.into(),
            members: vec![],
            display_color: [0, 0, 0],
        });
    }
    let [g1, g2] = [0, 1].map(|i| patch.groups[i].id);
    let d = destination_of(&patch, &[], &[g2, g1, g2, Uuid::new_v4()]).unwrap();
    assert_eq!((d.zones, d.groups), (vec![], vec![g1, g2]));
    assert!(destination_of(&patch, &[], &[Uuid::new_v4()]).is_err());
}

#[test]
fn a_poisoned_lock_is_taken_not_a_panic() {
    let m = Arc::new(Mutex::new(5));
    let m2 = Arc::clone(&m);
    let _ = std::thread::spawn(move || {
        let _g = m2.lock().unwrap();
        panic!("a command panicked while holding the state");
    })
    .join();
    assert!(m.is_poisoned());
    *m.locked() += 1;
    assert_eq!(*m.locked(), 6, "the state is still usable");
}

#[test]
fn track_names_and_moves_never_panic() {
    let mut show = prisma_engine::demo::demo_show();
    let name = next_track_name(&show);
    assert!(!show.tracks.iter().any(|t| t.name == name));
    let first = show.tracks[0].id;
    // Before a track that is not there: nothing moves.
    let before: Vec<_> = show.tracks.iter().map(|t| t.id).collect();
    move_track(&mut show.tracks, first, Some(Uuid::new_v4()));
    assert_eq!(show.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), before);
}

#[test]
fn a_new_group_gets_a_colour_no_group_has() {
    let g = |c: [u8; 3]| Group {
        id: Uuid::new_v4(),
        name: String::new(),
        members: vec![],
        display_color: c,
    };
    assert_eq!(next_group_color(&[]), GROUP_COLORS[0]);
    // The first free one, even with a gap left by a deleted or recoloured group.
    let taken = [g(GROUP_COLORS[0]), g(GROUP_COLORS[2])];
    assert_eq!(next_group_color(&taken), GROUP_COLORS[1]);
    // All taken: the palette repeats.
    let all: Vec<Group> = GROUP_COLORS.iter().map(|&c| g(c)).collect();
    assert_eq!(next_group_color(&all), GROUP_COLORS[0]);
}
