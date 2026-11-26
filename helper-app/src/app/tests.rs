use super::*;
use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
};
use slint::{LogicalPosition, PhysicalSize, Rgb8Pixel};

const WINDOW_WIDTH: i32 = 620;
const WINDOW_HEIGHT: i32 = 440;

/// Click targets in the 620×440 palette, in logical pixels. Update these when
/// the layout moves.
mod at {
    pub type Point = (f32, f32);

    /// Settings → Appearance theme dropdown, and the Light entry of its open list.
    pub const THEME_DROPDOWN: Point = (505., 136.);
    pub const THEME_POPUP_LIGHT: Point = (505., 138.);
    /// Results list: first row's name, first and second rows' play/pause buttons, and
    /// the middle of the first row's progress line.
    pub const FIRST_ROW_NAME: Point = (60., 104.);
    pub const FIRST_ROW_PREVIEW_BUTTON: Point = (586., 104.);
    pub const SECOND_ROW_PREVIEW_BUTTON: Point = (586., 141.);
    pub const PREVIEW_TRACK_MIDDLE: Point = (310., 118.);
    /// Footer gear that opens Settings.
    pub const SETTINGS_GEAR: Point = (597., 423.);
    /// Settings tabs and controls.
    pub const HOTKEYS_TAB: Point = (105., 66.);
    pub const ABOUT_TAB: Point = (313., 66.);
    pub const LAUNCH_HOTKEY_BUTTON: Point = (533., 125.);
    pub const FIRST_ACTION_RECORD: Point = (530., 141.);
    pub const FIRST_ACTION_CLEAR: Point = (581., 141.);
}

struct TestPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn palette_outline_reaches_all_four_window_edges() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let (width, height) = (WINDOW_WIDTH as usize, WINDOW_HEIGHT as usize);
    adapter.set_size(PhysicalSize::new(width as u32, height as u32));
    ui.show().unwrap();

    let mut pixels = vec![Rgb8Pixel::default(); width * height];
    for (theme, outline) in [
        (PaletteTheme::Light, [0x8b, 0x95, 0xa5]),
        (PaletteTheme::Dark, [0x54, 0x54, 0x54]),
        (PaletteTheme::MidnightIndigo, [0x36, 0x39, 0x52]),
        (PaletteTheme::Premiere, [0x50, 0x50, 0x50]),
    ] {
        apply_palette_theme(&ui, theme);
        assert!(adapter.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, width);
        }));
        let [r, g, b] = outline;
        let border = Rgb8Pixel { r, g, b };
        for (x, y) in [
            (0, height / 2),
            (width - 1, height / 2),
            (width / 2, 0),
            (width / 2, height - 1),
        ] {
            assert_eq!(pixels[y * width + x], border, "{theme:?} outline at ({x}, {y})");
        }
    }
}

#[test]
fn appearance_dropdown_saves_theme_and_recovers_from_save_failure() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
    ui.show().unwrap();
    ui.set_settings_open(true);

    let dir =
        std::env::temp_dir().join(format!("premiere-spotlight-theme-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.json");
    let target = Rc::new(RefCell::new(path.clone()));
    let saved = Rc::new(RefCell::new(AppSettings {
        auto_preview_audio: true,
        ..AppSettings::default()
    }));
    let weak = ui.as_weak();
    ui.on_select_theme({
        let (target, saved) = (target.clone(), saved.clone());
        move |label| {
            change_palette_theme(
                &weak.unwrap(),
                &mut saved.borrow_mut(),
                label.as_str(),
                &target.borrow(),
            );
        }
    });
    let weak = ui.as_weak();
    ui.on_toggle_settings(move || {
        let ui = weak.unwrap();
        ui.set_settings_open(!ui.get_settings_open());
    });

    let window = ui.window();
    let click = |(x, y): (f32, f32)| {
        let position = LogicalPosition::new(x, y);
        for event in [
            WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            },
            WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            },
        ] {
            window.dispatch_event(event);
        }
    };
    let key = |key: Key| {
        window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    };

    key(Key::RightArrow);
    key(Key::RightArrow);
    key(Key::RightArrow);
    key(Key::Tab);
    key(Key::Return);
    key(Key::DownArrow);
    assert_eq!(ui.get_theme_name(), "Premiere");
    assert_eq!(*saved.borrow(), settings::load_settings_from(&path));
    key(Key::Escape);
    assert!(ui.get_settings_open());

    click(at::THEME_DROPDOWN);
    click(at::THEME_POPUP_LIGHT);
    assert_eq!(ui.get_theme_name(), "Light");

    *target.borrow_mut() = dir.clone();
    click(at::THEME_DROPDOWN);
    key(Key::DownArrow);
    assert_eq!(ui.get_theme_name(), "Light");
    assert_eq!(saved.borrow().theme, PaletteTheme::Light);
    assert!(ui.get_settings_feedback_error());

    *target.borrow_mut() = path.clone();
    key(Key::DownArrow);
    assert_eq!(
        ui.get_theme_name(),
        "Dark",
        "the dropdown restored Light after the error"
    );
    assert!(!ui.get_settings_feedback_error());
    key(Key::Escape);
    assert!(ui.get_settings_open());
    key(Key::Escape);
    assert!(!ui.get_settings_open());

    let reloaded = AppSettings {
        theme: PaletteTheme::Premiere,
        ..saved.borrow().clone()
    };
    settings::save_settings_to(&path, &reloaded).unwrap();
    *saved.borrow_mut() = settings::load_settings_from(&path);
    apply_palette_theme(&ui, saved.borrow().theme);
    assert_eq!(ui.get_theme_name(), "Premiere");
    assert_eq!(ui.get_caption_color(), Color::from_rgb_u8(0x25, 0x25, 0x25));
    ui.set_settings_open(true);
    key(Key::RightArrow);
    key(Key::RightArrow);
    key(Key::RightArrow);
    click(at::THEME_DROPDOWN);
    key(Key::UpArrow);
    assert_eq!(
        ui.get_theme_name(),
        "Midnight Indigo",
        "the dropdown reflected the reloaded theme"
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
}

#[test]
fn audio_preview_resolves_only_the_settled_selection_and_ignores_stale_replies() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let item = |id: &str| ProjectItemEntry {
        item_id: id.into(),
        name: if id == "first" { "Renamed clip".into() } else { format!("{id}.wav") },
        bin_path: "Audio".into(),
        kind: ProjectItemKind::Media,
        offline: false,
        media_type: None,
        media_path: None,
    };
    let catalog = MediaCatalog {
        peer_id: 7,
        project_id: Some("project".into()),
        sequence_id: None,
        items: vec![item("first"), item("second")],
    };
    let bridge = BridgeClients::new();
    let outbound = bridge.connect_test_peer(catalog.clone());
    let mut preview = AudioPreview::default();
    let select = |preview: &mut AudioPreview, index: usize| {
        preview.select(&ui, Some(&catalog), Some(PreviewItem::Project(&catalog.items[index])), true);
    };
    select(&mut preview, 0);
    preview.poll(&ui, &bridge, Some(&catalog));
    assert!(outbound.try_recv().is_err());
    select(&mut preview, 1);
    assert!(ui.get_preview_active() && ui.get_preview_can_toggle());
    assert!(!ui.get_preview_playing());
    std::thread::sleep(Duration::from_millis(260));
    preview.poll(&ui, &bridge, Some(&catalog));
    let request: serde_json::Value = serde_json::from_str(&outbound.try_recv().unwrap()).unwrap();
    assert_eq!(request["type"], "request_audio_preview");
    assert_eq!(request["projectId"], "project");
    assert_eq!(request["itemId"], "second");
    assert_eq!(request.get("sequenceId"), None);
    select(&mut preview, 0);
    preview.resolved(&ui, bridge::AudioPreviewResult {
        peer_id: 7,
        request_id: request["requestId"].as_u64().unwrap(),
        source: Some(crate::protocol::AudioPreviewSource {
            path: "stale.wav".into(),
            in_seconds: None,
            out_seconds: None,
        }),
        message: String::new(),
    });
    assert!(!ui.get_preview_active());

    preview.toggle(&ui, &bridge, Some(&catalog));
    let manual: serde_json::Value = serde_json::from_str(&outbound.try_recv().unwrap()).unwrap();
    assert_eq!(manual["itemId"], "first");
    preview.cancel(&ui);
    preview.resolved(&ui, bridge::AudioPreviewResult {
        peer_id: 7,
        request_id: manual["requestId"].as_u64().unwrap(),
        source: None,
        message: "Stale error".into(),
    });
    assert!(!ui.get_status_message().as_str().contains("Stale error"));

    preview.select(&ui, Some(&catalog), Some(PreviewItem::Project(&catalog.items[0])), false);
    std::thread::sleep(Duration::from_millis(260));
    preview.poll(&ui, &bridge, Some(&catalog));
    let identify: serde_json::Value = serde_json::from_str(&outbound.try_recv().unwrap()).unwrap();
    preview.resolved(&ui, bridge::AudioPreviewResult {
        peer_id: 7,
        request_id: identify["requestId"].as_u64().unwrap(),
        source: Some(crate::protocol::AudioPreviewSource {
            path: "renamed.wav".into(),
            in_seconds: None,
            out_seconds: None,
        }),
        message: String::new(),
    });
    assert!(ui.get_preview_active() && ui.get_preview_can_toggle());
    assert!(!ui.get_preview_playing());
    assert!(ui.get_preview_message().is_empty());
}

#[test]
fn inline_audio_controls_fill_from_the_left_and_keep_row_activation_separate() {
    use std::cell::Cell;
    use slint::platform::PointerEventButton;

    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let row = |name: &str| EffectItemUI {
        name: name.into(),
        detail: "Audio".into(),
        icon: "audio".into(),
        tint: Color::from_rgb_u8(125, 211, 252),
        ..EffectItemUI::default()
    };
    ui.set_results(ModelRc::new(VecModel::from(vec![
        row("Music.wav"),
        row("SFX.wav"),
    ])));
    ui.set_total_matches(2);
    ui.set_preview_active(true);
    ui.set_preview_time("0:01 / 0:04".into());
    ui.set_preview_progress(0.25);
    ui.set_preview_can_toggle(true);
    ui.set_preview_can_seek(true);
    adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
    ui.show().unwrap();

    let width = WINDOW_WIDTH as usize;
    let mut pixels = vec![Rgb8Pixel::default(); width * WINDOW_HEIGHT as usize];
    assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
    let fill = Rgb8Pixel { r: 0xa5, g: 0xb4, b: 0xfc };
    assert_eq!(pixels[119 * width + 14], fill);
    assert_eq!(pixels[119 * width + 605], Rgb8Pixel { r: 0x3e, g: 0x42, b: 0x5a });
    assert_ne!(pixels[119 * width + 400], fill);
    assert_ne!(pixels[165 * width + 80], fill);
    // The progress fill and track meet the outline on y=121.
    assert_eq!(pixels[120 * width + 40], fill);
    assert_eq!(pixels[120 * width + 580], Rgb8Pixel { r: 0x3e, g: 0x42, b: 0x5a });

    let corner_pixels = |pixels: &[Rgb8Pixel]| {
        (119..122)
            .flat_map(|y| (6..12).chain(608..614).map(move |x| pixels[y * width + x]))
            .collect::<Vec<_>>()
    };
    let corners_with_track = corner_pixels(&pixels);
    ui.set_preview_can_seek(false);
    assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
    assert_eq!(corner_pixels(&pixels), corners_with_track);
    ui.set_preview_can_seek(true);
    ui.set_preview_progress(1.0);
    assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
    assert_eq!(corner_pixels(&pixels), corners_with_track);
    assert_eq!(pixels[120 * width + 605], fill);
    ui.set_preview_progress(0.25);
    let icon = Rgb8Pixel { r: 0xc7, g: 0xd2, b: 0xfe };
    let icon_pixels = |pixels: &[Rgb8Pixel]| {
        (92..116).flat_map(|y| &pixels[y * width + 574..y * width + 599])
            .filter(|&&pixel| pixel == icon).count()
    };
    assert!(icon_pixels(&pixels) > 0);
    ui.set_preview_playing(true);
    assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
    assert!(icon_pixels(&pixels) > 0);

    for (theme, fill, track) in [
        (PaletteTheme::Light, [0x24, 0x5a, 0xc3], [0xaa, 0xb8, 0xce]),
        (PaletteTheme::Dark, [0xe0, 0xe0, 0xe0], [0x69, 0x69, 0x69]),
        (PaletteTheme::Premiere, [0x67, 0xb6, 0xff], [0x53, 0x61, 0x6f]),
    ] {
        apply_palette_theme(&ui, theme);
        assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
        let [r, g, b] = fill;
        assert_eq!(pixels[119 * width + 14], Rgb8Pixel { r, g, b });
        let [r, g, b] = track;
        assert_eq!(pixels[119 * width + 605], Rgb8Pixel { r, g, b });
    }

    let played = Rc::new(Cell::new(false));
    let position = Rc::new(Cell::new(None));
    let activated = Rc::new(Cell::new(None));
    let played_cb = played.clone();
    let seek_cb = position.clone();
    let activated_cb = activated.clone();
    ui.on_toggle_preview_playback(move || played_cb.set(true));
    ui.on_seek_audio_preview(move |fraction| seek_cb.set(Some(fraction)));
    ui.on_activate_selected(|| panic!("preview control activated the search result"));
    ui.on_activate_index(move |index| activated_cb.set(Some(index)));

    let click = |(x, y): (f32, f32)| {
        let position = LogicalPosition::new(x, y);
        let button = PointerEventButton::Left;
        ui.window().dispatch_event(WindowEvent::PointerMoved { position });
        ui.window().dispatch_event(WindowEvent::PointerPressed { position, button });
        ui.window().dispatch_event(WindowEvent::PointerReleased { position, button });
    };
    click(at::FIRST_ROW_PREVIEW_BUTTON);
    assert!(played.get());
    click(at::PREVIEW_TRACK_MIDDLE);
    assert!(
        position.get().is_some_and(|value| (0.35..0.55).contains(&value)),
        "click produced seek position {:?}",
        position.get(),
    );
    assert_eq!(activated.get(), None);
    ui.set_preview_can_toggle(false);
    played.set(false);
    click(at::FIRST_ROW_PREVIEW_BUTTON);
    assert!(!played.get());
    assert_eq!(activated.get(), None);
    click(at::SECOND_ROW_PREVIEW_BUTTON);
    assert_eq!(activated.get(), Some(1));
    click(at::FIRST_ROW_NAME);
    assert_eq!(activated.get(), Some(0));
}

#[test]
fn error_toast_renders_a_separate_notification() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let (width, height) = (320, 56);
    adapter.set_size(PhysicalSize::new(width as u32, height as u32));
    let toast = Rc::new(ErrorToast::default());
    toast.show("Select at least one timeline clip first.".into(), 0);

    let mut pixels = vec![Rgb8Pixel::default(); width * height];
    assert!(adapter.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width);
    }));
    // The red accent runs down the left edge, and the message is light text beside the icon.
    assert_eq!(pixels[width * (height / 2) + 1], Rgb8Pixel { r: 0xe0, g: 0x72, b: 0x7e });
    assert!(pixels.chunks_exact(width).any(|row| {
        row[40..width - 12].iter().any(|pixel| pixel.g > 160)
    }));
}

#[test]
fn premiere_context_dims_rows_and_blocks_them_without_hiding() {
    use slint::Model;

    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let bridge = BridgeClients::new();
    let requests = bridge.connect_test_peer(MediaCatalog {
        peer_id: 7,
        project_id: Some("project".into()),
        sequence_id: Some("sequence".into()),
        items: Vec::new(),
    });
    let mut state = AppState::new(AppSettings::default());
    let read_command = || -> serde_json::Value {
        serde_json::from_str(&requests.try_recv().unwrap()).unwrap()
    };
    let open = |ui: &SpotlightWindow, state: &mut AppState| {
        center_and_show(ui, state, &bridge);
        assert_eq!(read_command()["type"], "request_project_items");
        let request = read_command();
        assert_eq!(request["type"], "request_context");
        ui.set_active_catalog("actions".into());
        ui.set_search_text("Remove Attributes".into());
        refresh_results_ui(ui, state);
        request["requestId"].as_u64().unwrap()
    };
    let first_row = |ui: &SpotlightWindow| ui.get_results().row_data(0).unwrap();
    let context = |total: u32| PremiereContext {
        project_id: Some("project".into()),
        sequence_id: Some("sequence".into()),
        sequence_name: Some("Sequence 01".into()),
        selection: Some(crate::protocol::SelectionSummary { total, ..Default::default() }),
        adjustment_layer_registered: None,
        host_version: Some("25.6.0".into()),
    };
    let reply = |request_id: u64, context: Option<PremiereContext>| ContextEvent {
        peer_id: 7,
        request_id,
        context,
    };

    let request_id = open(&ui, &mut state);
    // Rows stay enabled until Premiere answers, and stale replies are ignored.
    assert!(!first_row(&ui).disabled);
    receive_premiere_context(&ui, &mut state, reply(request_id + 100, Some(context(0))));
    assert!(!first_row(&ui).disabled);

    receive_premiere_context(&ui, &mut state, reply(request_id, Some(context(0))));
    let row = first_row(&ui);
    assert!(row.disabled);
    assert_eq!(row.subtitle, "Select clips in the Timeline first.");

    // Enter on the dimmed row explains instead of sending the command.
    assert!(apply_entry_at(0, &ui, &mut state, &bridge).unwrap().is_none());
    assert!(state.is_visible);
    assert!(state.pending_action.is_none());
    assert_eq!(ui.get_status_message(), "Select clips in the Timeline first.");
    assert_eq!(ui.get_flash_row(), 0);
    assert!(requests.try_recv().is_err());

    // Reopening forgets the old selection; a null context leaves rows enabled.
    let request_id = open(&ui, &mut state);
    assert!(!first_row(&ui).disabled);
    assert_eq!(ui.get_flash_row(), -1);
    receive_premiere_context(&ui, &mut state, reply(request_id, None));
    assert!(!first_row(&ui).disabled);

    let request_id = open(&ui, &mut state);
    receive_premiere_context(&ui, &mut state, reply(request_id, Some(context(2))));
    assert!(!first_row(&ui).disabled);
    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    assert!(!state.is_visible);
    assert_eq!(read_command()["action"], "remove_attributes");
}

#[test]
fn accepted_actions_hide_immediately_and_errors_leave_visibility_alone() {
    use slint::Model;

    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let bridge = BridgeClients::new();
    let effect = SearchResult::Effect(Arc::new(EffectEntry {
        name: "Gaussian Blur".into(),
        match_name: "blur".into(),
        effect_type: "videoEffect".into(),
        folder: String::new(),
    }));
    let item = catalog::ProjectItemEntry {
        item_id: "clip".into(),
        name: "Clip".into(),
        bin_path: "Footage".into(),
        kind: ProjectItemKind::Media,
        offline: false,
        media_type: None,
        media_path: None,
    };
    let catalog = MediaCatalog {
        peer_id: 7,
        project_id: Some("project".into()),
        sequence_id: Some("sequence".into()),
        items: vec![item.clone()],
    };
    let mut state = AppState {
        matched_items: vec![effect, SearchResult::ProjectItem(Arc::new(item))],
        media_catalog: Some(catalog.clone()),
        ..AppState::new(AppSettings::default())
    };
    show_palette(&ui, &mut state);
    for index in [0, 1] {
        let message = apply_entry_at(index, &ui, &mut state, &bridge).unwrap_err();
        assert!(state.is_visible);
        assert!(message.contains("bridge disconnected"));
    }

    state.catalog.set_project_items(catalog.items.clone());
    let requests = bridge.connect_test_peer(catalog);
    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    assert!(!state.is_visible);
    assert_eq!(state.pending_effects.len(), 1);
    assert!(requests.try_recv().unwrap().contains("apply_effect"));

    show_palette(&ui, &mut state);
    ui.set_active_catalog("actions".into());
    ui.set_search_text("Remove Attributes".into());
    refresh_results_ui(&ui, &mut state);
    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    assert!(!state.is_visible);
    let request_id = state.pending_action.as_ref().unwrap().request_id;
    let command: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
    assert_eq!(command["type"], "run_action");
    assert_eq!(command["action"], "remove_attributes");
    assert_eq!(command["requestId"], request_id);
    let toast = Rc::new(ErrorToast::default());
    for peer_id in [8, 7] {
        finish_action(&ui, &mut state, Some(CommandReply {
            peer_id, request_id, ok: true, message: "Removed effects".into(),
        }), &toast);
        assert_eq!(state.pending_action.is_some(), peer_id == 8);
    }
    assert_eq!(ui.get_status_message().as_str(), "Removed effects");
    assert!(state.recent_items.is_empty());

    for (catalog, action, wire_name) in [
        ("actions", Action::RegisterAdjustmentLayer, "register_adjustment_layer"),
        ("all", Action::AddAdjustmentLayer, "add_adjustment_layer"),
    ] {
        show_palette(&ui, &mut state);
        ui.set_active_catalog(catalog.into());
        ui.set_search_text(action.name().into());
        refresh_results_ui(&ui, &mut state);
        apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
        assert!(!state.is_visible);
        let command: serde_json::Value =
            serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
        let pending = state.pending_action.as_ref().unwrap();
        assert_eq!(command["type"], "run_action");
        assert_eq!(command["action"], wire_name);
        assert_eq!(command["requestId"], pending.request_id);
        let request_id = pending.request_id;
        finish_action(&ui, &mut state, Some(CommandReply {
            peer_id: 7, request_id, ok: true, message: "Done".into(),
        }), &toast);
    }

    show_palette(&ui, &mut state);
    ui.set_active_catalog("all".into());
    ui.set_search_text("go to marker".into());
    refresh_results_ui(&ui, &mut state);
    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    assert!(state.is_visible && ui.get_marker_picker());
    assert_eq!(ui.get_marker_status().as_str(), "Loading markers…");

    let read_command = || -> serde_json::Value {
        serde_json::from_str(&requests.try_recv().unwrap()).unwrap()
    };
    let list_reply = |request: &serde_json::Value| {
        let json = serde_json::json!({
            "type": "marker_ack",
            "requestId": request["requestId"],
            "result": {
                "kind": "list",
                "projectId": "project",
                "projectName": "Editing project",
                "markers": [
                    { "name": "Cue", "comments": "Later", "ticks": "508032000000", "sequenceId": "sequence", "sequenceName": "Selects" },
                    { "name": "", "comments": "", "ticks": "0", "sequenceId": "nested", "sequenceName": "Nested" },
                    { "name": "Cue", "comments": "Earlier", "ticks": "254016000000", "sequenceId": "sequence", "sequenceName": "Selects" }
                ]
            },
            "message": ""
        });
        let crate::protocol::IncomingPluginMessage::MarkerAck { request_id, result, message } =
            serde_json::from_value(json).unwrap()
        else { panic!("Expected a marker reply") };
        MarkerEvent { peer_id: 7, request_id, result, message }
    };
    let discarded = read_command();
    assert_eq!(discarded["operation"], "list");
    escape_palette(&ui, &mut state);
    assert!(state.is_visible && !ui.get_marker_picker());
    assert_eq!(ui.get_search_text().as_str(), "go to marker");
    assert_eq!(ui.get_active_catalog().as_str(), "all");

    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    let active = read_command();
    receive_marker(&ui, &mut state, &bridge, list_reply(&discarded), &toast);
    assert_eq!(ui.get_marker_status().as_str(), "Loading markers…");
    receive_marker(&ui, &mut state, &bridge, list_reply(&active), &toast);
    assert_eq!(ui.get_marker_context().as_str(), "Editing project");
    let results = ui.get_results();
    assert_eq!(results.row_count(), 3);
    assert_eq!(results.row_data(0).unwrap().name.as_str(), "Unnamed marker");
    assert!(results.row_data(0).unwrap().detail.starts_with("Nested · "));
    assert_eq!(results.row_data(1).unwrap().detail.as_str(), "Selects · 00:00:01.000");
    assert!(results.row_data(2).unwrap().detail.ends_with(" · 00:00:02.000"));
    assert!(results.row_data(0).unwrap().icon == "marker");
    ui.set_search_text("Earlier".into());
    refresh_marker_results(&ui, &mut state);
    assert_eq!(ui.get_results().row_count(), 1);

    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    let seek = read_command();
    assert_eq!(seek["operation"], "seek");
    assert_eq!(seek["projectId"], "project");
    assert_eq!(seek["marker"]["sequenceId"], "sequence");
    assert_eq!(seek["marker"]["ticks"], "254016000000");
    receive_marker(&ui, &mut state, &bridge, MarkerEvent {
        peer_id: 7,
        request_id: seek["requestId"].as_u64().unwrap(),
        result: Some(crate::protocol::MarkerReply::Sought),
        message: String::new(),
    }, &toast);
    assert!(!state.is_visible && !ui.get_marker_picker());

    let mut picker = MarkerPicker::open(&bridge, "".into());
    let list = read_command();
    picker.receive(list_reply(&list));
    let mut other_timeline = MediaCatalog {
        peer_id: 7,
        project_id: Some("project".into()),
        sequence_id: Some("other-sequence".into()),
        items: vec![],
    };
    assert!(!picker.refresh_if_project_changed(&bridge, &other_timeline));
    other_timeline.project_id = Some("other-project".into());
    assert!(picker.refresh_if_project_changed(&bridge, &other_timeline));
    assert!(picker.markers().is_empty());
    assert!(picker.context_label.is_empty());
    assert_eq!(picker.status(), "Loading markers…");
    assert_eq!(read_command()["operation"], "list");
    assert!(!picker.accepts(&list_reply(&list)));

    show_palette(&ui, &mut state);
    ui.set_active_catalog("project".into());
    ui.set_search_text("Clip".into());
    refresh_results_ui(&ui, &mut state);
    for success in [true, false] {
        apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
        assert!(!state.is_visible);
        assert!(requests.try_recv().unwrap().contains("place_project_item"));
        let request_id = state.pending_placement.as_ref().unwrap().request.request_id;
        if success {
            // A success reply must leave a manually reopened palette alone.
            show_palette(&ui, &mut state);
        }
        let error = update_placement(
            &ui,
            &mut state,
            Some(CommandReply {
                peer_id: 7,
                request_id,
                ok: success,
                message: if success { "Placed Clip" } else { "Timeline changed" }.into(),
            }),
        );
        assert_eq!(state.is_visible, success);
        assert_eq!(
            error.as_deref(),
            if success { None } else { Some("Timeline changed") },
        );
        assert_eq!(ui.get_search_text().as_str(), "Clip");
        assert_eq!(state.recent_items.len(), 1);
    }

    activate_action(Action::RemoveAttributes, &ui, &mut state, &bridge).unwrap();
    assert_eq!(read_command()["action"], "remove_attributes");
    assert!(!state.is_visible);
    activate_action(Action::GoToMarker, &ui, &mut state, &bridge).unwrap();
    assert!(state.is_visible && ui.get_marker_picker());
    assert_eq!(read_command()["type"], "request_project_items");
    assert_eq!(read_command()["type"], "request_context");
    assert_eq!(read_command()["operation"], "list");
    escape_palette(&ui, &mut state);
    assert!(state.is_visible && !ui.get_marker_picker());
    assert_eq!(ui.get_active_catalog(), "actions");

    activate_action(Action::LockHorizontalAxis, &ui, &mut state, &bridge).unwrap();
    assert!(state.is_visible && ui.get_settings_open());
    assert_eq!(ui.get_settings_tab_index(), 1);
    assert!(requests.try_recv().is_err());

    ui.set_settings_open(false);
    state
        .settings
        .action_hotkeys
        .insert(Action::LockHorizontalAxis.id().into(), "F8".into());
    activate_action(Action::LockHorizontalAxis, &ui, &mut state, &bridge).unwrap();
    assert!(state.is_visible && !ui.get_settings_open());
    assert!(ui.get_status_message().as_str().contains("Hold F8"));
    assert!(requests.try_recv().is_err());
}

#[test]
fn shift_activate_handles_effects_and_project_items() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
    let ui = SpotlightWindow::new().unwrap();
    ui.show().unwrap();
    let window = ui.window();
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));
    ui.invoke_focus_search();

    let shift_selected = Rc::new(std::cell::Cell::new(0u32));
    let shift_clicked = Rc::new(std::cell::RefCell::new(Vec::new()));
    let activated = Rc::new(std::cell::Cell::new(0u32));
    let clicked = Rc::new(std::cell::Cell::new(0u32));
    {
        let shift_selected = shift_selected.clone();
        ui.on_shift_activate_selected(move || shift_selected.set(shift_selected.get() + 1));
        let shift_clicked = shift_clicked.clone();
        ui.on_shift_activate_index(move |idx| shift_clicked.borrow_mut().push(idx));
        let activated = activated.clone();
        ui.on_activate_selected(move || activated.set(activated.get() + 1));
        let clicked = clicked.clone();
        ui.on_activate_index(move |_| clicked.set(clicked.get() + 1));
    }
    let press = |key: Key| {
        window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    };
    press(Key::Return);
    assert_eq!((shift_selected.get(), activated.get()), (0, 1));
    window.dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
    press(Key::Return);
    window.dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
    assert_eq!((shift_selected.get(), activated.get()), (1, 1));

    let effect = SearchResult::Effect(Arc::new(EffectEntry {
        name: "Gaussian Blur".into(),
        match_name: "blur".into(),
        effect_type: "videoEffect".into(),
        folder: String::new(),
    }));
    let item = |id: &str, kind| catalog::ProjectItemEntry {
        item_id: id.into(),
        name: id.into(),
        bin_path: "Footage".into(),
        kind,
        offline: false,
        media_type: None,
        media_path: None,
    };
    let catalog = MediaCatalog {
        peer_id: 7,
        project_id: Some("project".into()),
        sequence_id: None,
        items: vec![item("clip", ProjectItemKind::Media), item("bin", ProjectItemKind::Bin)],
    };
    let mut state = AppState {
        matched_items: vec![
            effect,
            SearchResult::ProjectItem(Arc::new(catalog.items[0].clone())),
            SearchResult::ProjectItem(Arc::new(catalog.items[1].clone())),
        ],
        media_catalog: Some(catalog.clone()),
        ..AppState::new(AppSettings::default())
    };
    sync_viewport_slice(&ui, &mut state);

    // Shift+Click on a result row invokes shift_activate_index with that row index.
    let row_pos = LogicalPosition::new(120., 111.);
    let button = PointerEventButton::Left;
    window.dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
    window.dispatch_event(WindowEvent::PointerPressed { position: row_pos, button });
    window.dispatch_event(WindowEvent::PointerReleased { position: row_pos, button });
    window.dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
    assert_eq!(&*shift_clicked.borrow(), &[0]);
    assert_eq!(clicked.get(), 0);

    let bridge = BridgeClients::new();
    let requests = bridge.connect_test_peer(catalog);

    // Shift+Enter/Click on an effect applies it and requests opening Effect Controls on ok=true.
    show_palette(&ui, &mut state);
    shift_activate_at(0, &ui, &mut state, &bridge).unwrap();
    assert!(!state.is_visible);
    let msg: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
    assert_eq!(msg["type"], "apply_effect");
    let req_id = msg["requestId"].as_u64().unwrap();
    assert!(state.pending_effects.get(&req_id).unwrap().open_effect_controls);
    assert_eq!(
        complete_effect(
            &mut state,
            &ApplyResult {
                peer_id: 7,
                request_id: Some(req_id),
                ok: false,
                message: "Select a clip first.".into(),
            }
        ),
        None
    );

    show_palette(&ui, &mut state);
    shift_activate_at(0, &ui, &mut state, &bridge).unwrap();
    let msg: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
    let req_id = msg["requestId"].as_u64().unwrap();
    assert_eq!(
        complete_effect(
            &mut state,
            &ApplyResult {
                peer_id: 7,
                request_id: Some(req_id),
                ok: true,
                message: r#"Applied "Gaussian Blur" to 1 video clip."#.into(),
            }
        ),
        Some(true)
    );

    // Regular Enter/Click applies without requesting Effect Controls.
    show_palette(&ui, &mut state);
    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    let msg: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
    let req_id = msg["requestId"].as_u64().unwrap();
    assert_eq!(
        complete_effect(
            &mut state,
            &ApplyResult {
                peer_id: 7,
                request_id: Some(req_id),
                ok: true,
                message: r#"Applied "Gaussian Blur" to 1 video clip."#.into(),
            }
        ),
        Some(false)
    );

    // Bins are rejected without hiding, and the reason shows in the footer.
    show_palette(&ui, &mut state);
    assert!(shift_activate_at(2, &ui, &mut state, &bridge).unwrap().is_none());
    assert!(ui.get_status_message().contains("Bins"));
    assert_eq!(ui.get_flash_row(), 2);
    assert!(state.is_visible);
    assert!(requests.try_recv().is_err());

    for success in [true, false] {
        show_palette(&ui, &mut state);
        shift_activate_at(1, &ui, &mut state, &bridge).unwrap();
        assert!(!state.is_visible);
        assert!(requests.try_recv().unwrap().contains("open_in_source_monitor"));
        let request_id = state.pending_open.as_ref().unwrap().request.request_id;
        let error = update_open(&ui, &mut state, Some(CommandReply {
            peer_id: 7,
            request_id,
            ok: success,
            message: if success { "Opened clip" } else { "Too old" }.into(),
        }));
        assert_eq!(error.as_deref(), if success { None } else { Some("Too old") });
        assert!(state.pending_open.is_none());
        assert!(state.pending_placement.is_none());
    }

    shift_activate_at(1, &ui, &mut state, &bridge).unwrap();
    state.pending_open.as_mut().unwrap().request.deadline = Instant::now();
    assert!(update_open(&ui, &mut state, None).unwrap().contains("didn't respond"));
}

#[test]
fn catalog_tabs_cycle_and_reopen_to_recent_items() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
    ui.show().unwrap();
    let window = ui.window();
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));
    ui.set_search_text("Take".into());
    ui.invoke_focus_search();
    let press_tab = || {
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::Tab.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::Tab.into() });
    };
    // Centers of the All, Effects, Project, Library and Actions text tabs.
    const TAB_X: [f32; 5] = [23., 70., 129., 188., 248.];
    let click_tab = |index: usize| {
        let position = LogicalPosition::new(TAB_X[index], 66.);
        let button = PointerEventButton::Left;
        window.dispatch_event(WindowEvent::PointerPressed { position, button });
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
    };
    // Only the active tab has an underline along the bottom of the tab row.
    let assert_highlight = |selected: usize| {
        let width = WINDOW_WIDTH as usize;
        let mut pixels = vec![Rgb8Pixel::default(); width * WINDOW_HEIGHT as usize];
        window.request_redraw();
        assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
        let underline = Rgb8Pixel { r: 0x81, g: 0x8c, b: 0xf8 };
        for (index, x) in TAB_X.iter().enumerate() {
            let pixel = pixels[81 * width + *x as usize];
            assert_eq!(pixel == underline, index == selected, "tab {index} underline");
        }
    };
    assert_highlight(0);
    for (index, expected) in [(1, "effects"), (2, "project"), (3, "library"), (4, "actions"), (0, "all")] {
        press_tab();
        assert_eq!(ui.get_active_catalog().as_str(), expected);
        assert_eq!(ui.get_search_text().as_str(), "Take");
        assert_highlight(index);
    }
    click_tab(4);
    assert_eq!(ui.get_active_catalog().as_str(), "actions");
    assert_highlight(4);
    click_tab(3);
    assert_eq!(ui.get_active_catalog().as_str(), "library");
    assert_highlight(3);
    click_tab(2);
    assert_eq!(ui.get_active_catalog().as_str(), "project");
    assert_highlight(2);
    click_tab(1);
    assert_eq!(ui.get_active_catalog().as_str(), "effects");
    click_tab(0);
    assert_eq!(ui.get_active_catalog().as_str(), "all");
    click_tab(1);
    press_tab();
    assert_eq!(ui.get_active_catalog().as_str(), "project");
    assert_eq!(ui.get_search_text().as_str(), "Take");
    let mut state = AppState {
        is_visible: true,
        ..AppState::new(AppSettings::default())
    };
    let effect = Arc::new(EffectEntry {
        name: "Gaussian Blur".into(),
        match_name: "blur".into(),
        effect_type: "videoEffect".into(),
        folder: String::new(),
    });
    let pending = |request_id| PendingEffect {
        entry: effect.clone(),
        request: PendingRequest::new(7, request_id),
        open_effect_controls: false,
    };
    let ack = |request_id, ok| ApplyResult {
        peer_id: 7,
        request_id,
        ok,
        message: r#"Applied "Gaussian Blur" to 1 video clip."#.into(),
    };
    state.pending_effects.insert(1, pending(1));
    assert_eq!(complete_effect(&mut state, &ack(Some(2), true)), None);
    assert_eq!(complete_effect(&mut state, &ack(Some(1), false)), None);
    assert!(state.recent_items.is_empty());
    state.pending_effects.insert(3, pending(3));
    state.pending_effects.insert(4, pending(4));
    assert_eq!(complete_effect(&mut state, &ack(None, true)), None);
    assert_eq!(complete_effect(&mut state, &ack(Some(3), true)), Some(false));
    let mut other_peer = ack(None, true);
    other_peer.peer_id = 8;
    assert_eq!(complete_effect(&mut state, &other_peer), None);
    assert_eq!(complete_effect(&mut state, &ack(None, true)), Some(false));
    assert_eq!(complete_effect(&mut state, &ack(None, true)), None);
    state.pending_effects.insert(5, pending(5));
    let mut foreign = ack(Some(5), true);
    foreign.peer_id = 8;
    assert_eq!(complete_effect(&mut state, &foreign), None);
    assert_eq!(complete_effect(&mut state, &ack(Some(5), true)), Some(false));
    ui.set_search_text(" take ".into());
    hide_and_restore_focus(&ui, &mut state);
    let bridge = BridgeClients::new();
    center_and_show(&ui, &mut state, &bridge);
    assert_eq!(ui.get_active_catalog().as_str(), "all");
    assert!(ui.get_search_text().is_empty());
    assert_eq!(window.size().height, WINDOW_HEIGHT as u32);
    assert!(matches!(state.matched_items.as_slice(),
        [SearchResult::Effect(entry)] if entry.name == "Gaussian Blur"));

    let item = catalog::ProjectItemEntry {
        item_id: "audio".into(),
        name: "achievement_sfx.mp3".into(),
        bin_path: "Audio".into(),
        kind: ProjectItemKind::Media,
        offline: false,
        media_type: None,
        media_path: None,
    };
    remember_item(&mut state, RecentItem {
        item: SearchResult::ProjectItem(Arc::new(item.clone())), project_id: Some("project".into()),
    });
    state.media_catalog = Some(MediaCatalog {
        peer_id: 1,
        project_id: Some("project".into()),
        sequence_id: Some("sequence".into()),
        items: vec![item],
    });
    refresh_results_ui(&ui, &mut state);
    assert!(matches!(state.matched_items.as_slice(),
        [SearchResult::ProjectItem(item), SearchResult::Effect(_)]
            if item.name == "achievement_sfx.mp3"));
    state.media_catalog.as_mut().unwrap().project_id = Some("different project".into());
    refresh_results_ui(&ui, &mut state);
    assert_eq!(state.matched_items.len(), 1);

    ui.set_search_text("".into());
    press_tab();
    press_tab();
    assert_eq!(ui.get_active_catalog().as_str(), "project");

    state.catalog.set_project_items((1..=8).map(|i| catalog::ProjectItemEntry {
        item_id: i.to_string(),
        name: format!("Project item {i}"),
        bin_path: "Footage / Interview".into(),
        kind: catalog::ProjectItemKind::Media,
        offline: false,
        media_type: None,
        media_path: None,
    }).collect());
    refresh_results_ui(&ui, &mut state);
    assert_eq!(ui.get_total_matches(), 8);
    click_tab(0);
    click_tab(1);
    click_tab(2);
    assert_eq!(ui.get_active_catalog().as_str(), "project");
    show_status(&ui, "This project item can't be placed on the timeline.");

    let width = WINDOW_WIDTH as usize;
    let mut pixels = vec![Rgb8Pixel::default(); width * WINDOW_HEIGHT as usize];
    window.request_redraw();
    assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
    for row in 0..8 {
        let top = (88 + row * 36) * width;
        let bottom = top + 34 * width;
        assert!(pixels[top..bottom].iter().any(|pixel| pixel.r > 170), "row {row} should render its label");
    }
    assert!(pixels[404 * width..].iter().any(|pixel| pixel.r > 130), "the footer should render its text");
    let height = WINDOW_HEIGHT as usize;
    let outline = Rgb8Pixel { r: 0x36, g: 0x39, b: 0x52 };
    for edge in [
        [width / 2, width + width / 2, 2 * width + width / 2],
        std::array::from_fn(|i| (height - 1 - i) * width + width / 2),
        std::array::from_fn(|i| height / 2 * width + i),
        std::array::from_fn(|i| height / 2 * width + width - 1 - i),
    ] {
        assert!(edge.iter().any(|&i| pixels[i] == outline), "outline missing at a palette edge");
    }

    // Escape should close the palette when search has focus and after clicking elsewhere inside the palette.
    let close_count = Rc::new(std::cell::Cell::new(0u32));
    {
        let close_count = close_count.clone();
        ui.on_close_requested(move || {
            close_count.set(close_count.get() + 1);
        });
    }
    ui.invoke_focus_search();
    window.dispatch_event(WindowEvent::KeyPressed { text: Key::Escape.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: Key::Escape.into() });
    assert_eq!(close_count.get(), 1);
    let footer_pos = LogicalPosition::new(300., 423.);
    window.dispatch_event(WindowEvent::PointerPressed {
        position: footer_pos,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position: footer_pos,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::KeyPressed { text: Key::Escape.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: Key::Escape.into() });
    assert_eq!(close_count.get(), 2);

    // Clicking outside (including back onto Premiere) hides once activation succeeded or grace expires.
    let shown_at = Instant::now();
    state.is_visible = true;
    state.shown_at = Some(shown_at);
    state.saw_foreground = false;
    assert!(matches!(
        evaluate_palette_focus(&mut state, false, shown_at + Duration::from_millis(20)),
        FocusOutcome::RetryForeground
    ));
    assert!(state.is_visible);
    assert!(matches!(
        evaluate_palette_focus(&mut state, true, shown_at + Duration::from_millis(40)),
        FocusOutcome::KeepVisible
    ));
    assert!(state.saw_foreground);
    assert!(matches!(
        evaluate_palette_focus(&mut state, false, shown_at + Duration::from_millis(60)),
        FocusOutcome::HideLostFocus
    ));
    assert!(!state.is_visible);

    state.is_visible = true;
    state.shown_at = Some(shown_at);
    state.saw_foreground = false;
    assert!(matches!(
        evaluate_palette_focus(&mut state, false, shown_at + ACTIVATION_GRACE),
        FocusOutcome::HideLostFocus
    ));
    assert!(!state.is_visible);

    // Reopening after focus-loss dismissal or hide_and_restore_focus must take one toggle.
    toggle_palette_from_hotkey_for_fg(&ui, &mut state, &bridge, 4242, true);
    assert!(state.is_visible);
    assert_eq!(state.previous_hwnd, 4242);
    assert!(state.shown_at.is_some());

    // Closing via hide_and_restore_focus clears visibility, then one toggle reopens it.
    state.previous_hwnd = 0;
    hide_and_restore_focus(&ui, &mut state);
    assert!(!state.is_visible);
    assert!(state.shown_at.is_none());
    assert!(!state.saw_foreground);

    toggle_palette_from_hotkey_for_fg(&ui, &mut state, &bridge, 4242, true);
    assert!(state.is_visible);
    assert_eq!(state.previous_hwnd, 4242);
    assert!(state.shown_at.is_some());

    // If the OS minimized/hid the palette (palette_actively_open = false) while Premiere is active,
    // one toggle must reopen and re-anchor to Premiere rather than silently hiding.
    state.previous_hwnd = 1111;
    state.shown_at = None;
    toggle_palette_from_hotkey_with_focus(&ui, &mut state, &bridge, 4242, true, false);
    assert!(state.is_visible);
    assert_eq!(state.previous_hwnd, 4242);
    assert!(state.shown_at.is_some());

    // Pressing toggle while the palette is actually visible and active closes it cleanly.
    toggle_palette_from_hotkey_with_focus(&ui, &mut state, &bridge, 9999, false, true);
    assert!(!state.is_visible);
    assert!(state.shown_at.is_none());

    // Stale is_visible while Premiere is inactive clears state cleanly.
    state.is_visible = true;
    toggle_palette_from_hotkey_with_focus(&ui, &mut state, &bridge, 9999, false, false);
    assert!(!state.is_visible);
}

#[test]
fn f12_toggles_debug_log_window() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let _ = slint::platform::set_platform(Box::new(TestPlatform(adapter.clone())));
    adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
    let ui = SpotlightWindow::new().unwrap();
    ui.show().unwrap();
    let window = ui.window();
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));

    let toggled = Rc::new(std::cell::Cell::new(0u32));
    {
        let toggled = toggled.clone();
        ui.on_toggle_debug_log(move || toggled.set(toggled.get() + 1));
    }

    let press_f12 = || {
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::F12.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::F12.into() });
    };

    // Works both when search-input has focus and when FocusScope handles it.
    ui.invoke_focus_search();
    press_f12();
    assert_eq!(toggled.get(), 1);

    let footer_pos = LogicalPosition::new(300., 423.);
    window.dispatch_event(WindowEvent::PointerPressed {
        position: footer_pos,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position: footer_pos,
        button: PointerEventButton::Left,
    });
    press_f12();
    assert_eq!(toggled.get(), 2);

    // DebugLogController records logs and toggles open/closed cleanly.
    debug_log::log_event(LogKind::Info, "TEST", "F12 debug window check");
    let ctrl = Rc::new(DebugLogController::default());
    ctrl.toggle(0);
    assert!(ctrl.is_open());
    ctrl.toggle(0);
    assert!(!ctrl.is_open());
}

#[test]
fn moving_selection_inside_visible_window_reuses_results_model() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let _ = slint::platform::set_platform(Box::new(TestPlatform(adapter)));
    let ui = SpotlightWindow::new().unwrap();
    let mut store = CatalogStore::new_with_defaults();
    store.set_project_items(
        (1..=12)
            .map(|i| catalog::ProjectItemEntry {
                item_id: i.to_string(),
                name: format!("Clip {i:02}"),
                bin_path: "Footage".into(),
                kind: ProjectItemKind::Media,
                offline: false,
                media_type: None,
                media_path: None,
            })
            .collect(),
    );
    let mut state = AppState {
        catalog: store,
        ..AppState::new(AppSettings::default())
    };
    use slint::Model;
    let model_ptr = |ui: &SpotlightWindow| -> *const VecModel<EffectItemUI> {
        ui.get_results()
            .as_any()
            .downcast_ref::<VecModel<EffectItemUI>>()
            .unwrap() as *const VecModel<EffectItemUI>
    };
    ui.set_active_catalog("project".into());
    refresh_results_ui(&ui, &mut state);
    let initial_ptr = model_ptr(&ui);

    state.selected_abs_index = 2;
    sync_viewport_slice(&ui, &mut state);
    assert_eq!(model_ptr(&ui), initial_ptr);
    assert_eq!(ui.get_selected_index(), 2);

    state.selected_abs_index = VISIBLE_ROWS;
    sync_viewport_slice(&ui, &mut state);
    assert_ne!(model_ptr(&ui), initial_ptr);
    assert_eq!(state.scroll_offset, 1);
    assert_eq!(ui.get_selected_index(), (VISIBLE_ROWS - 1) as i32);
}

#[test]
fn bin_drill_down_enters_and_steps_out_without_hiding() {
    use slint::Model;

    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let bridge = BridgeClients::new();
    let item = |id: &str, name: &str, bin_path: &str, kind: ProjectItemKind| ProjectItemEntry {
        item_id: id.into(),
        name: name.into(),
        bin_path: bin_path.into(),
        kind,
        offline: false,
        media_type: None,
        media_path: None,
    };
    let items = vec![
        item("b1", "Footage", "Project root", ProjectItemKind::Bin),
        item("b2", "B-Roll", "Footage", ProjectItemKind::Bin),
        item("c1", "Drone.mp4", "Footage / B-Roll", ProjectItemKind::Media),
        item("c2", "Music.wav", "Audio", ProjectItemKind::Media),
    ];
    let catalog = MediaCatalog {
        peer_id: 7,
        project_id: Some("proj".into()),
        sequence_id: Some("seq".into()),
        items: items.clone(),
    };
    let requests = bridge.connect_test_peer(catalog.clone());
    let mut store = CatalogStore::new_with_defaults();
    store.set_project_items(items);
    let mut state = AppState {
        catalog: store,
        media_catalog: Some(catalog),
        ..AppState::new(AppSettings::default())
    };

    show_palette(&ui, &mut state);
    ui.set_search_text("Footage".into());
    refresh_results_ui(&ui, &mut state);
    let footage_idx = state
        .matched_items
        .iter()
        .position(|result| matches!(result, SearchResult::ProjectItem(e) if e.item_id == "b1"))
        .unwrap();

    apply_entry_at(footage_idx, &ui, &mut state, &bridge).unwrap();
    assert!(state.is_visible);
    assert!(requests.try_recv().is_err());
    assert!(state.pending_placement.is_none());
    assert_eq!(state.active_bin_path.as_deref(), Some("Footage"));
    assert_eq!(ui.get_active_bin().as_str(), "Footage");
    assert_eq!(ui.get_active_catalog().as_str(), "project");
    assert!(ui.get_search_text().is_empty());
    assert_eq!(state.matched_items.len(), 2);

    let broll_idx = state
        .matched_items
        .iter()
        .position(|result| matches!(result, SearchResult::ProjectItem(e) if e.item_id == "b2"))
        .unwrap();
    apply_entry_at(broll_idx, &ui, &mut state, &bridge).unwrap();
    assert_eq!(state.active_bin_path.as_deref(), Some("Footage / B-Roll"));
    assert_eq!(ui.get_active_bin().as_str(), "Footage / B-Roll");
    assert_eq!(state.matched_items.len(), 1);
    assert_eq!(
        ui.get_results().row_data(0).unwrap().detail,
        "Footage › B-Roll"
    );

    step_out_bin(&ui, &mut state);
    assert_eq!(state.active_bin_path.as_deref(), Some("Footage"));
    assert_eq!(ui.get_active_bin().as_str(), "Footage");
    assert_eq!(state.matched_items.len(), 2);

    step_out_bin(&ui, &mut state);
    assert!(state.active_bin_path.is_none());
    assert!(ui.get_active_bin().is_empty());
    assert_eq!(state.matched_items.len(), 4);

    let footage_idx = state
        .matched_items
        .iter()
        .position(|result| matches!(result, SearchResult::ProjectItem(e) if e.item_id == "b1"))
        .unwrap();
    apply_entry_at(footage_idx, &ui, &mut state, &bridge).unwrap();
    assert_eq!(state.active_bin_path.as_deref(), Some("Footage"));
    center_and_show(&ui, &mut state, &bridge);
    assert!(state.active_bin_path.is_none());
    assert!(ui.get_active_bin().is_empty());
}

#[test]
fn backspace_on_empty_search_triggers_step_out_bin_in_slint() {
    use std::cell::Cell;

    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
    ui.show().unwrap();
    let window = ui.window();
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));

    let step_outs = Rc::new(Cell::new(0usize));
    let step_outs_cb = step_outs.clone();
    ui.on_step_out_bin(move || {
        step_outs_cb.set(step_outs_cb.get() + 1);
    });

    let press_backspace = || {
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Backspace.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Backspace.into(),
        });
    };

    ui.set_active_bin("Footage".into());
    ui.set_search_text("".into());
    ui.invoke_focus_search();
    for ch in ["a", "b"] {
        window.dispatch_event(WindowEvent::KeyPressed { text: ch.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: ch.into() });
    }
    assert_eq!(ui.get_search_text().as_str(), "ab");

    press_backspace();
    assert_eq!(step_outs.get(), 0);
    assert_eq!(ui.get_search_text().as_str(), "a");

    press_backspace();
    assert_eq!(step_outs.get(), 0);
    assert_eq!(ui.get_search_text().as_str(), "");

    press_backspace();
    assert_eq!(step_outs.get(), 1);
}

#[test]
fn row_detail_sits_at_the_right_edge_after_the_kind_icon_and_name() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
    ui.show().unwrap();
    let window = ui.window();
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));

    let render = |name: &str, detail: &str| -> Vec<Rgb8Pixel> {
        let item = EffectItemUI {
            name: name.into(),
            detail: detail.into(),
            icon: "sequence".into(),
            tint: Color::from_rgb_u8(0xdd, 0xd6, 0xfe),
            ..EffectItemUI::default()
        };
        ui.set_results(ModelRc::new(VecModel::from(vec![item])));
        ui.set_selected_index(0);
        let width = WINDOW_WIDTH as usize;
        let mut pixels = vec![Rgb8Pixel::default(); width * WINDOW_HEIGHT as usize];
        window.request_redraw();
        assert!(adapter.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, width);
        }));
        pixels
    };
    let width = WINDOW_WIDTH as usize;
    // The first row spans y=88..122; its kind icon sits at x=14..40.
    let diff_xs = |a: &[Rgb8Pixel], b: &[Rgb8Pixel]| -> Vec<usize> {
        (89..121)
            .flat_map(|y| (8..612).filter(move |&x| a[y * width + x] != b[y * width + x]))
            .collect()
    };

    let plain = render("00_BACK UP", "");
    assert!((96..114).any(|y| (14..40).any(|x| plain[y * width + x] != plain[100 * width + 300])),
        "the kind icon should draw at the left of the row");
    let with_detail = render("00_BACK UP", "04_Nests › 041_Main");
    let changed = diff_xs(&plain, &with_detail);
    assert!(!changed.is_empty());
    let first = *changed.iter().min().unwrap();
    assert!(first > 380, "the detail should be right-aligned, got min x={first}");

    // A long name elides before the detail and never touches the icon.
    let long_name = "Very_Long_Sequence_Name_That_Must_Elide_Cleanly_Before_The_Detail_Text";
    let long_plain = render(long_name, "");
    let long_detail = render(long_name, "01_Project › 04_Nests › 041_Main › Exports › Final_Master");
    let changed = diff_xs(&long_plain, &long_detail);
    assert!(changed.iter().all(|&x| x > 40), "the detail must not disturb the icon");
}

#[test]
fn default_effect_controls_setting_swaps_enter_and_shift_enter_for_effects() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let effect = SearchResult::Effect(Arc::new(EffectEntry {
        name: "Gaussian Blur".into(),
        match_name: "blur".into(),
        effect_type: "videoEffect".into(),
        folder: String::new(),
    }));
    let clip = catalog::ProjectItemEntry {
        item_id: "clip".into(),
        name: "Clip".into(),
        bin_path: "Footage".into(),
        kind: ProjectItemKind::Media,
        offline: false,
        media_type: None,
        media_path: None,
    };
    let catalog = MediaCatalog {
        peer_id: 7,
        project_id: Some("project".into()),
        sequence_id: Some("sequence".into()),
        items: vec![clip.clone()],
    };
    let bridge = BridgeClients::new();
    let requests = bridge.connect_test_peer(catalog.clone());
    let mut state = AppState {
        matched_items: vec![effect, SearchResult::ProjectItem(Arc::new(clip))],
        media_catalog: Some(catalog),
        settings: AppSettings {
            hotkey: "Alt+Space".into(),
            open_effect_controls_by_default: true,
            ..AppSettings::default()
        },
        ..AppState::new(AppSettings::default())
    };

    show_palette(&ui, &mut state);
    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    let msg: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
    let req1 = msg["requestId"].as_u64().unwrap();
    assert!(state.pending_effects.get(&req1).unwrap().open_effect_controls);

    show_palette(&ui, &mut state);
    shift_activate_at(0, &ui, &mut state, &bridge).unwrap();
    let msg2: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
    let req2 = msg2["requestId"].as_u64().unwrap();
    assert!(!state.pending_effects.get(&req2).unwrap().open_effect_controls);

    // Project items still open in Source Monitor on Shift+Enter and place on Enter.
    show_palette(&ui, &mut state);
    shift_activate_at(1, &ui, &mut state, &bridge).unwrap();
    assert!(requests.try_recv().unwrap().contains("open_in_source_monitor"));
}

#[test]
fn settings_gear_toggles_in_palette_settings_view_and_esc_returns_to_search() {
    use std::cell::Cell;

    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
    ui.show().unwrap();
    let window = ui.window();
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));

    let closed = Rc::new(Cell::new(0usize));
    let closed_cb = closed.clone();
    ui.on_close_requested(move || closed_cb.set(closed_cb.get() + 1));

    let ui_weak = ui.as_weak();
    ui.on_toggle_settings(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_settings_open(!ui.get_settings_open());
        }
    });

    let captured = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let captured_cb = captured.clone();
    ui.on_capture_shortcut(move |target, text, ctrl, alt, shift, meta| {
        if let Ok(Some((_, label))) =
            settings::hotkey_from_slint_event(text.as_str(), ctrl, alt, shift, meta)
        {
            captured_cb.borrow_mut().push((target.to_string(), label));
        }
    });

    let click = |(x, y): (f32, f32)| {
        let position = LogicalPosition::new(x, y);
        window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    };
    click(at::SETTINGS_GEAR);
    assert!(ui.get_settings_open());

    click(at::LAUNCH_HOTKEY_BUTTON);
    assert!(ui.get_recording_hotkey());
    assert_eq!(ui.get_recording_target().as_str(), "launch");

    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Alt.into(),
    });
    window.dispatch_event(WindowEvent::KeyPressed {
        text: " ".into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: " ".into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Alt.into(),
    });
    assert_eq!(
        &*captured.borrow(),
        &[("launch".to_string(), "Alt+Space".to_string())]
    );

    // About cancels recording and hides the shortcut controls.
    click(at::ABOUT_TAB);
    assert!(!ui.get_recording_hotkey());
    click(at::LAUNCH_HOTKEY_BUTTON);
    assert!(!ui.get_recording_hotkey());
    assert!(ui.get_settings_open());

    for _ in 0..4 {
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::LeftArrow.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::LeftArrow.into(),
        });
    }
    click(at::LAUNCH_HOTKEY_BUTTON);
    assert!(ui.get_recording_hotkey());

    // Esc exits recording first, keeping Settings open; second Esc exits Settings without closing Spotlight.
    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Escape.into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Escape.into(),
    });
    assert!(!ui.get_recording_hotkey());
    assert!(ui.get_settings_open());
    assert_eq!(closed.get(), 0);

    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Escape.into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Escape.into(),
    });
    assert!(!ui.get_settings_open());
    assert_eq!(closed.get(), 0);

    ui.set_action_hotkeys(ModelRc::new(VecModel::from(vec![ActionHotkeyUI {
        id: Action::AddAdjustmentLayer.id().into(),
        label: Action::AddAdjustmentLayer.name().into(),
        hint: Action::AddAdjustmentLayer.hotkey_hint().into(),
        shortcut: "F6".into(),
    }])));
    let cleared = Rc::new(RefCell::new(SharedString::default()));
    let cleared_cb = cleared.clone();
    ui.on_clear_shortcut(move |id| *cleared_cb.borrow_mut() = id);
    click(at::SETTINGS_GEAR);
    click(at::HOTKEYS_TAB);
    click(at::FIRST_ACTION_RECORD);
    assert_eq!(ui.get_recording_target(), Action::AddAdjustmentLayer.id());
    window.dispatch_event(WindowEvent::KeyPressed { text: Key::F7.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: Key::F7.into() });
    assert_eq!(captured.borrow().last().unwrap().1, "F7");
    click(at::FIRST_ACTION_CLEAR);
    assert_eq!(cleared.borrow().as_str(), Action::AddAdjustmentLayer.id());
}

#[test]
fn configurable_modifier_settings_hotkey_and_catalog_shortcuts_work() {
    use std::cell::Cell;

    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
    ui.show().unwrap();
    let window = ui.window();
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));

    let clip = catalog::ProjectItemEntry {
        item_id: "clip".into(),
        name: "Clip".into(),
        bin_path: "01_Footage".into(),
        kind: ProjectItemKind::Media,
        offline: false,
        media_type: None,
        media_path: None,
    };
    let state = Rc::new(RefCell::new(AppState {
        matched_items: vec![SearchResult::ProjectItem(Arc::new(clip))],
        active_bin_path: Some("01_Footage".into()),
        settings: AppSettings {
            settings_hotkey: "Ctrl+,".into(),
            catalog_effects_hotkey: "Ctrl+2".into(),
            ..AppSettings::default()
        },
        is_visible: true,
        shown_at: Some(Instant::now()),
        saw_foreground: true,
        ..AppState::new(AppSettings::default())
    }));
    {
        let mut st = state.borrow_mut();
        ui.set_active_catalog("project".into());
        ui.set_active_bin("01_Footage".into());
        ui.set_settings_hotkey_label(st.settings.settings_hotkey.clone().into());
        ui.set_catalog_effects_hotkey_label(st.settings.catalog_effects_hotkey.clone().into());
        refresh_results_ui(&ui, &mut st);
    }

    let primary_calls = Rc::new(Cell::new(0usize));
    let alternate_calls = Rc::new(Cell::new(0usize));
    let primary_cb = primary_calls.clone();
    ui.on_activate_selected(move || primary_cb.set(primary_cb.get() + 1));
    let alt_cb = alternate_calls.clone();
    ui.on_shift_activate_selected(move || alt_cb.set(alt_cb.get() + 1));

    let ui_weak = ui.as_weak();
    ui.on_toggle_settings(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_settings_open(!ui.get_settings_open());
        }
    });

    let ui_weak = ui.as_weak();
    let state_ref = state.clone();
    ui.on_try_palette_shortcut(move |text, ctrl, alt, shift, meta, _repeat| {
        let Some(ui) = ui_weak.upgrade() else {
            return false;
        };
        let settings_open = ui.get_settings_open();
        let action = {
            let st = state_ref.borrow();
            settings::match_palette_shortcut(
                &st.settings,
                settings_open,
                text.as_str(),
                ctrl,
                alt,
                shift,
                meta,
            )
        };
        match action {
            Some(PaletteShortcutAction::ToggleSettings) => {
                ui.set_settings_open(!settings_open);
                true
            }
            Some(PaletteShortcutAction::SelectCatalog(target)) => {
                let mut st = state_ref.borrow_mut();
                set_catalog(&ui, target);
                if target != Catalog::Project {
                    clear_bin_scope(&ui, &mut st);
                }
                true
            }
            Some(PaletteShortcutAction::PreviewAudio | PaletteShortcutAction::RunAction(_)) => false,
            None => false,
        }
    });

    // Switch alternate action modifier to Ctrl: Ctrl+Enter fires shift-activate, Shift+Enter fires primary.
    ui.set_alternate_modifier("ctrl".into());
    ui.invoke_focus_search();
    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Return.into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Return.into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    assert_eq!(alternate_calls.get(), 1);
    assert_eq!(primary_calls.get(), 0);

    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Shift.into(),
    });
    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Return.into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Return.into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Shift.into(),
    });
    assert_eq!(alternate_calls.get(), 1);
    assert_eq!(primary_calls.get(), 1);

    // Ctrl+, toggles Settings open and closed even while search input had focus.
    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    window.dispatch_event(WindowEvent::KeyPressed {
        text: ",".into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: ",".into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    assert!(ui.get_settings_open());

    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    window.dispatch_event(WindowEvent::KeyPressed {
        text: ",".into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: ",".into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    assert!(!ui.get_settings_open());

    // Configured direct catalog shortcut Ctrl+2 jumps straight to Effects and clears Project bin scope.
    ui.invoke_focus_search();
    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    window.dispatch_event(WindowEvent::KeyPressed {
        text: "2".into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: "2".into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    assert_eq!(ui.get_active_catalog().as_str(), "effects");
    assert!(state.borrow().active_bin_path.is_none());
    assert_eq!(ui.get_search_text().as_str(), "");
}

/// Toggles the palette as the launch hotkey would, treating `fg_hwnd` as the foreground window.
fn toggle_palette_from_hotkey_for_fg(
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
    fg_hwnd: isize,
    is_premiere_fg: bool,
) {
    toggle_palette_from_hotkey_with_focus(
        ui,
        state,
        bridge,
        fg_hwnd,
        is_premiere_fg,
        state.is_visible,
    );
}

#[test]
fn alternate_action_opens_sequences_in_the_timeline() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let item = |id: &str, kind| catalog::ProjectItemEntry {
        item_id: id.into(),
        name: id.into(),
        bin_path: "Sequences".into(),
        kind,
        offline: false,
        media_type: None,
        media_path: None,
    };
    let catalog = MediaCatalog {
        peer_id: 7,
        project_id: Some("project".into()),
        sequence_id: Some("main".into()),
        items: vec![
            item("Draft", ProjectItemKind::Sequence),
            item("Main", ProjectItemKind::ActiveSequence),
            item("Clip", ProjectItemKind::Media),
        ],
    };
    let bridge = BridgeClients::new();
    let requests = bridge.connect_test_peer(catalog.clone());
    let mut state = AppState {
        matched_items: catalog
            .items
            .iter()
            .map(|entry| SearchResult::ProjectItem(Arc::new(entry.clone())))
            .collect(),
        media_catalog: Some(catalog),
        ..AppState::new(AppSettings::default())
    };
    let read_command = || -> serde_json::Value {
        serde_json::from_str(&requests.try_recv().unwrap()).unwrap()
    };

    for (index, item_id) in [(0, "Draft"), (1, "Main")] {
        show_palette(&ui, &mut state);
        shift_activate_at(index, &ui, &mut state, &bridge).unwrap();
        assert!(!state.is_visible);
        let command = read_command();
        assert_eq!(command["type"], "open_sequence");
        assert_eq!(command["itemId"], item_id);
        assert_eq!(command["projectId"], "project");
        let pending = state.pending_open.as_ref().unwrap();
        assert_eq!(pending.target, OpenTarget::Timeline);
        let request_id = pending.request.request_id;
        assert!(update_open(&ui, &mut state, Some(CommandReply {
            peer_id: 7, request_id, ok: true, message: format!("Opened \"{item_id}\" in the Timeline."),
        })).is_none());
        assert!(state.pending_open.is_none());
        assert!(ui.get_status_message().contains("in the Timeline"));
    }

    // Media still opens in the Source Monitor.
    show_palette(&ui, &mut state);
    shift_activate_at(2, &ui, &mut state, &bridge).unwrap();
    assert_eq!(read_command()["type"], "open_in_source_monitor");
    assert_eq!(state.pending_open.as_ref().unwrap().target, OpenTarget::SourceMonitor);

    // Failures and timeouts name the Timeline.
    show_palette(&ui, &mut state);
    shift_activate_at(0, &ui, &mut state, &bridge).unwrap();
    let request_id = read_command()["requestId"].as_u64().unwrap();
    let error = update_open(&ui, &mut state, Some(CommandReply {
        peer_id: 7, request_id, ok: false, message: String::new(),
    }));
    assert_eq!(error.as_deref(), Some(OpenTarget::Timeline.failure()));
    shift_activate_at(0, &ui, &mut state, &bridge).unwrap();
    state.pending_open.as_mut().unwrap().request.deadline = Instant::now();
    assert_eq!(update_open(&ui, &mut state, None).as_deref(), Some(OpenTarget::Timeline.timeout()));

    // The bridge refuses to open anything but a sequence in the Timeline.
    let catalog = state.media_catalog.clone().unwrap();
    assert!(bridge.open_sequence(&catalog, "Clip".into()).unwrap_err().contains("Only sequences"));
}

#[test]
fn add_named_marker_takes_a_name_and_adds_it_without_the_dialog() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let bridge = BridgeClients::new();
    let requests = bridge.connect_test_peer(MediaCatalog {
        peer_id: 7,
        project_id: Some("project".into()),
        sequence_id: Some("sequence".into()),
        items: Vec::new(),
    });
    let mut state = AppState::new(AppSettings::default());
    let toast = Rc::new(ErrorToast::default());
    let drain = || while requests.try_recv().is_ok() {};

    // The hotkey opens Spotlight straight into the name field.
    activate_action(Action::AddNamedMarker, &ui, &mut state, &bridge).unwrap();
    drain();
    assert!(state.is_visible && ui.get_marker_naming());
    assert_eq!(ui.get_active_catalog(), "actions");
    assert_eq!((ui.get_hint_enter().as_str(), ui.get_hint_escape().as_str()), ("Add marker", "Back"));

    // Typing names the marker instead of searching, and Esc returns to the search.
    ui.set_search_text("Intro".into());
    refresh_results_ui(&ui, &mut state);
    assert_eq!(ui.get_total_matches(), 0);
    escape_palette(&ui, &mut state);
    assert!(state.is_visible && !ui.get_marker_naming());
    assert_eq!(ui.get_search_text(), "");
    assert!(ui.get_total_matches() > 0);

    // From the ACT list, Enter on the row opens the name field and keeps the query.
    ui.set_search_text("named marker".into());
    refresh_results_ui(&ui, &mut state);
    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    assert!(state.is_visible && ui.get_marker_naming());
    ui.set_search_text("  Intro  ".into());
    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    assert!(!state.is_visible && !ui.get_marker_naming());
    let command: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
    assert_eq!(command["type"], "marker_command");
    assert_eq!(command["operation"], "add");
    assert_eq!(command["name"], "Intro");
    let request_id = command["requestId"].as_u64().unwrap();
    let reply = |request_id: u64, result: Option<MarkerReply>, message: &str| MarkerEvent {
        peer_id: 7,
        request_id,
        result,
        message: message.into(),
    };
    receive_marker(&ui, &mut state, &bridge, reply(request_id + 1, Some(MarkerReply::Added), ""), &toast);
    assert!(state.pending_named_marker.is_some());
    receive_marker(&ui, &mut state, &bridge, reply(request_id, Some(MarkerReply::Added), ""), &toast);
    assert!(state.pending_named_marker.is_none());
    assert_eq!(ui.get_status_message(), "Added marker \u{201c}Intro\u{201d}.");

    // Shift+Enter also submits; an error reply clears the request.
    activate_action(Action::AddNamedMarker, &ui, &mut state, &bridge).unwrap();
    drain();
    shift_activate_at(0, &ui, &mut state, &bridge).unwrap();
    let command: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
    assert_eq!(command["name"], "");
    let request_id = command["requestId"].as_u64().unwrap();
    receive_marker(&ui, &mut state, &bridge, reply(request_id, None, "A marker already exists"), &toast);
    assert!(state.pending_named_marker.is_none());

    // Without an open sequence the name field stays open and explains why.
    activate_action(Action::AddNamedMarker, &ui, &mut state, &bridge).unwrap();
    drain();
    state.premiere_context = Some(PremiereContext {
        project_id: Some("project".into()),
        ..PremiereContext::default()
    });
    apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
    assert!(state.is_visible && ui.get_marker_naming());
    assert_eq!(ui.get_status_message(), "Open a sequence first.");
    assert!(requests.try_recv().is_err());

    // Switching catalogs leaves the name field.
    clear_marker_picker(&ui, &mut state);
    assert!(!ui.get_marker_naming() && state.marker_naming.is_none());
}

#[test]
fn library_files_outside_the_project_get_an_import_badge_and_hints() {
    let file = |path: &str| {
        let path = std::path::PathBuf::from(path);
        crate::library::LibraryEntry {
            name: path.file_name().unwrap().to_str().unwrap().to_owned(),
            key: crate::library::path_key(path.to_str().unwrap()),
            folder: "SFX".into(),
            media_type: catalog::MediaType::Audio,
            path,
        }
    };
    let mut state = AppState::new(AppSettings::default());
    state.catalog.set_library_files(vec![file(r"D:\SFX\Boom.wav"), file(r"D:\SFX\Clap.wav")]);
    state.catalog.set_project_items(vec![ProjectItemEntry {
        item_id: "clap".into(),
        name: "Clap.wav".into(),
        bin_path: "Spotlight Library".into(),
        kind: ProjectItemKind::Media,
        offline: false,
        media_type: Some(catalog::MediaType::Audio),
        media_path: Some("d:/sfx/clap.wav".into()),
    }]);
    state.matched_items = state.catalog.search("", "library");
    let facts = state.facts();
    let rows: Vec<_> = state
        .matched_items
        .iter()
        .map(|result| result_row(result, &facts, &state.catalog))
        .collect();
    assert_eq!(rows.iter().map(|row| (row.name.as_str(), row.needs_import)).collect::<Vec<_>>(),
        [("Boom.wav", true), ("Clap.wav", false)]);
    assert_eq!(rows[1].detail.as_str(), "In project · SFX");

    state.selected_abs_index = 0;
    assert_eq!(key_hints(&state), ("Import and place at playhead", "Import to Source Monitor", "Close"));
    state.selected_abs_index = 1;
    assert_eq!(key_hints(&state), ("Place at playhead", "Open in Source Monitor", "Close"));
}

#[test]
fn footer_hints_follow_the_highlighted_row_and_messages_fade() {
    use slint::Model;

    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let effect = |kind: &str| SearchResult::Effect(Arc::new(EffectEntry {
        name: "Blur".into(),
        match_name: "blur".into(),
        effect_type: kind.into(),
        folder: String::new(),
    }));
    let item = |kind| SearchResult::ProjectItem(Arc::new(ProjectItemEntry {
        item_id: "i".into(),
        name: "Item".into(),
        bin_path: "Footage".into(),
        kind,
        offline: false,
        media_type: None,
        media_path: None,
    }));
    let mut state = AppState::new(AppSettings::default());
    let hints = |state: &mut AppState, result: SearchResult| {
        state.matched_items = vec![result];
        state.selected_abs_index = 0;
        state.results_dirty = true;
        sync_viewport_slice(&ui, state);
        (
            ui.get_hint_enter().to_string(),
            ui.get_hint_alternate().to_string(),
            ui.get_hint_escape().to_string(),
        )
    };
    let expect = |enter: &str, alternate: &str| (enter.to_string(), alternate.to_string(), "Close".to_string());

    assert_eq!(hints(&mut state, effect("videoEffect")), expect("Apply", "Apply and open Effect Controls"));
    assert_eq!(hints(&mut state, effect("preset")), expect("Find in Effects panel", ""));
    assert_eq!(hints(&mut state, item(ProjectItemKind::Media)), expect("Place at playhead", "Open in Source Monitor"));
    assert_eq!(hints(&mut state, item(ProjectItemKind::Sequence)), expect("Nest at playhead", "Open in Timeline"));
    assert_eq!(hints(&mut state, item(ProjectItemKind::Bin)), expect("Open bin", ""));
    assert_eq!(hints(&mut state, SearchResult::Action(Action::RemoveAttributes)), expect("Run", ""));
    // The active sequence can't nest into itself, so only the Timeline hint remains.
    assert_eq!(hints(&mut state, item(ProjectItemKind::ActiveSequence)), expect("", "Open in Timeline"));

    // The setting that swaps Enter and the alternate action swaps the hints too.
    state.settings.open_effect_controls_by_default = true;
    assert_eq!(hints(&mut state, effect("videoEffect")), expect("Apply and open Effect Controls", "Apply"));

    // A row Premiere can't run right now offers no Enter hint.
    state.premiere_context = Some(PremiereContext {
        project_id: Some("p".into()),
        sequence_id: Some("s".into()),
        selection: Some(crate::protocol::SelectionSummary { total: 0, ..Default::default() }),
        ..PremiereContext::default()
    });
    assert_eq!(hints(&mut state, SearchResult::Action(Action::RemoveAttributes)), expect("", ""));
    let row = ui.get_results().row_data(0).unwrap();
    assert!(row.disabled && row.icon == "action");
    assert_eq!(row.subtitle, "Select clips in the Timeline first.");

    // Naming a marker and browsing markers both go back with Esc.
    state.marker_naming = Some(MarkerNaming { previous_query: "".into() });
    assert_eq!(hints(&mut state, effect("videoEffect")), ("Add marker".into(), "".into(), "Back".into()));
    state.marker_naming = None;

    // Messages show in place of the hints until their timer hides them.
    assert!(!ui.get_status_visible());
    show_status(&ui, "Applied \"Blur\" to 1 clip.");
    assert!(ui.get_status_visible());
    assert_eq!(ui.get_status_message(), "Applied \"Blur\" to 1 clip.");
}

#[test]
fn each_kind_of_result_gets_its_own_icon_and_media_files_follow_their_type() {
    let effect = |kind: &str| SearchResult::Effect(Arc::new(EffectEntry {
        name: "E".into(),
        match_name: "e".into(),
        effect_type: kind.into(),
        folder: String::new(),
    }));
    let parse = |json: &str| -> ProjectItemEntry { serde_json::from_str(json).unwrap() };
    let item = |kind: &str, media_type: &str| SearchResult::ProjectItem(Arc::new(parse(&format!(
        r#"{{"itemId":"i","name":"I","binPath":"B","kind":"{kind}"{media_type}}}"#
    ))));
    let icon = |result: SearchResult| kind_icon(&result).name;

    assert_eq!(icon(effect("videoEffect")), "video-effect");
    assert_eq!(icon(effect("audioEffect")), "audio-effect");
    assert_eq!(icon(effect("videoTransition")), "video-transition");
    assert_eq!(icon(effect("audioTransition")), "audio-transition");
    assert_eq!(icon(effect("preset")), "preset");
    assert_eq!(icon(item("media", r#","mediaType":"video""#)), "video");
    assert_eq!(icon(item("media", r#","mediaType":"image""#)), "image");
    assert_eq!(icon(item("media", r#","mediaType":"audio""#)), "audio");
    // Older plugins, items without an extension and unknown types show as video.
    assert_eq!(icon(item("media", "")), "video");
    assert_eq!(icon(item("media", r#","mediaType":null"#)), "video");
    assert_eq!(icon(item("media", r#","mediaType":"hologram""#)), "video");
    assert_eq!(icon(item("sequence", "")), "sequence");
    assert_eq!(icon(item("activeSequence", "")), "sequence");
    assert_eq!(icon(item("bin", "")), "bin");
    assert_eq!(icon(item("other", "")), "other");
    assert_eq!(icon(SearchResult::Action(Action::AddMarker)), "action");

    // Every kind has a distinct look, apart from the deliberate sequence and video fallbacks.
    let looks: std::collections::HashSet<_> = [
        effect("videoEffect"), effect("audioEffect"), effect("videoTransition"), effect("audioTransition"),
        effect("preset"), item("media", r#","mediaType":"video""#), item("media", r#","mediaType":"image""#),
        item("media", r#","mediaType":"audio""#), item("sequence", ""), item("bin", ""),
        SearchResult::Action(Action::AddMarker), item("other", ""),
    ]
    .iter()
    .map(kind_icon)
    .map(|look| (look.name, look.tint))
    .collect();
    assert_eq!(looks.len(), 12);
}

#[test]
fn an_effect_is_refused_when_two_premiere_sessions_are_connected() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
    let ui = SpotlightWindow::new().unwrap();
    let bridge = BridgeClients::new();
    let session = |peer_id| MediaCatalog {
        peer_id,
        project_id: Some("project".into()),
        sequence_id: Some("sequence".into()),
        items: Vec::new(),
    };
    let first = bridge.connect_test_peer(session(1));
    let second = bridge.connect_test_peer(session(2));
    let mut state = AppState::new(AppSettings::default());
    state.is_visible = true;
    state.matched_items = vec![SearchResult::Effect(Arc::new(EffectEntry {
        name: "Gaussian Blur".into(),
        match_name: "blur".into(),
        effect_type: EffectType::VideoEffect,
        folder: String::new(),
    }))];

    let error = apply_entry_at(0, &ui, &mut state, &bridge).unwrap_err();
    assert_eq!(error, "Close the other Premiere session before applying effects.");
    assert!(first.try_recv().is_err() && second.try_recv().is_err());
    assert!(state.is_visible && state.pending_effects.is_empty());
}
