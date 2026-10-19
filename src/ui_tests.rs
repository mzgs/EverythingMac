use crate::{AppView, OpenSettings, search_view, settings::Settings, settings_view};
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, KeyBinding, Point, TestAppContext, Window,
    WindowBounds, WindowOptions, px, size,
};
use std::{fs, path::PathBuf, time::Duration};

struct Fixture {
    directory: PathBuf,
    root: PathBuf,
    settings: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "everything-ui-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let root = directory.join("files");
        fs::create_dir_all(root.join("Archive")).unwrap();
        for file in [
            "alpha.txt",
            "beta.JPG",
            "gamma.mp3",
            "Archive/alpha-old.txt",
        ] {
            fs::write(root.join(file), b"fixture").unwrap();
        }
        let settings = directory.join("settings.json");
        Settings {
            roots: vec![root.clone()],
            excluded: Vec::new(),
        }
        .save(&settings)
        .unwrap();
        Self {
            directory,
            root,
            settings,
        }
    }

    fn open(&self, cx: &mut TestAppContext) -> AnyWindowHandle {
        cx.update(gpui_kit::init);
        cx.update(crate::init_theme);
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.set_global(Settings {
                roots: vec![self.root.clone()],
                excluded: Vec::new(),
            });
            cx.bind_keys([
                KeyBinding::new("space", gpui_kit::NoAction, Some("Popover > Input")),
                KeyBinding::new("down", search_view::NextResult, Some("Everything")),
                KeyBinding::new("up", search_view::PreviousResult, Some("Everything")),
                KeyBinding::new("cmd-f", search_view::FocusSearch, Some("Everything")),
                KeyBinding::new("space", search_view::QuickLook, Some("FileResults")),
                KeyBinding::new("cmd-a", search_view::SelectAllResults, Some("FileResults")),
                KeyBinding::new("cmd-c", search_view::CopyFiles, Some("FileResults")),
                KeyBinding::new("shift-cmd-c", search_view::CopyPath, Some("Everything")),
                KeyBinding::new(
                    "cmd-backspace",
                    search_view::TrashResults,
                    Some("FileResults"),
                ),
                KeyBinding::new(
                    "shift-down",
                    search_view::ExtendNextResult,
                    Some("FileResults"),
                ),
                KeyBinding::new(
                    "shift-up",
                    search_view::ExtendPreviousResult,
                    Some("FileResults"),
                ),
                KeyBinding::new("cmd-,", OpenSettings, None),
            ]);
            let (handle, view) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1150.), px(800.)),
                    })),
                    ..gpui_kit::component::TitleBar::window_options()
                },
                cx,
                |window, cx| {
                    // Match main's renderer flag without mutating the parallel test process environment.
                    window.set_scroll_layers(false);
                    cx.new(|cx| {
                        AppView::new(
                            None,
                            None,
                            Some(self.directory.join("cache/index.sqlite3")),
                            window,
                            cx,
                        )
                    })
                },
            )
            .unwrap();
            settings_view::register_action(
                handle,
                view.read(cx).search.clone(),
                Some(self.settings.clone()),
                None,
                cx,
            );
            handle
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

async fn wait_count(cx: &mut TestAppContext, handle: AnyWindowHandle, count: usize) {
    let label = format!("{count} results");
    cx.wait_for(handle, Duration::from_secs(10), |window, _| {
        // The native index worker needs wall time while GPUI advances its test clock.
        std::thread::sleep(Duration::from_millis(2));
        window
            .try_find("results-table")
            .is_some_and(|table| table.label() == Some(label.as_str()))
    })
    .await;
}

async fn wait_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle, present: bool) {
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("dialog").is_some() == present
    })
    .await;
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn type_query(window: &mut Window, cx: &mut App, query: &str) {
    window.click("search-input", cx);
    window.press("cmd-a", cx);
    window.input(query, cx);
}

#[gpui_kit::test]
async fn long_path_tooltip_wraps_inside_the_window(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let parent = fixture.root.join("W".repeat(200));
    fs::create_dir(&parent).unwrap();
    fs::write(parent.join("tooltip-target.txt"), b"fixture").unwrap();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 8).await;
    cx.update_window(handle, |_, window, cx| {
        type_query(window, cx, "tooltip-target.txt");
    })
    .unwrap();
    wait_count(cx, handle, 1).await;
    cx.update_window(handle, |_, window, cx| {
        window.hover(("result-path", 0usize), cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("path-tooltip-content").is_some()
    })
    .await;
    cx.update_window(handle, |_, window, _| {
        let content = window.find("path-tooltip-content");
        assert_eq!(content.label(), parent.to_str());
        let bounds = content.bounds();
        assert!(
            bounds.size.height > px(40.),
            "Long paths must wrap: {bounds:?}"
        );
        assert!(bounds.size.width <= px(600.));
        assert!(bounds.left() >= px(0.));
        assert!(bounds.right() <= window.viewport_size().width);
    })
    .unwrap();
}

#[gpui_kit::test]
async fn duplicate_scan_groups_only_matching_files_and_returns_to_live_search(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("beta.JPG"), b"differs").unwrap();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.wait_for(handle, Duration::from_secs(10), |window, _| {
        std::thread::sleep(Duration::from_millis(2));
        window.find("find-duplicates").disabled() != Some(true)
    })
    .await;
    cx.update_window(handle, |_, window, cx| window.click("find-duplicates", cx))
        .unwrap();
    cx.wait_for(handle, Duration::from_secs(5), |window, _| {
        window.try_find("duplicate-summary").is_some()
    })
    .await;
    wait_count(cx, handle, 3).await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.find("duplicate-summary").label(),
            Some("1 duplicate groups, 3 files, 0 skipped")
        );
        window.click("Name", cx);
        assert!(window.try_find("duplicate-summary").is_some());
        assert!(
            window
                .find(("result", 0usize))
                .label()
                .unwrap()
                .starts_with("Duplicate group 1:")
        );
        assert_eq!(window.find("find-duplicates").checked(), Some(true));
        window.click(("result", 0usize), cx);
        window.click("copy-path", cx);
        assert!(!cx.read_from_clipboard().unwrap().text().unwrap().is_empty());
    })
    .unwrap();
    fs::remove_file(fixture.root.join("gamma.mp3")).unwrap();
    wait_count(cx, handle, 2).await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.find("duplicate-summary").label(),
            Some("1 duplicate groups, 2 files, 1 skipped")
        );
        type_query(window, cx, "alpha");
    })
    .unwrap();
    cx.run_until_parked();
    cx.background_executor
        .advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    wait_count(cx, handle, 2).await;
    cx.update_window(handle, |_, window, cx| {
        assert!(window.try_find("duplicate-summary").is_none());
        window.click("find-duplicates", cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(5), |window, _| {
        window.try_find("duplicate-summary").is_some()
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.find("duplicate-summary").label(),
            Some("1 duplicate groups, 2 files, 0 skipped")
        );
        window.click("close-duplicates", cx);
    })
    .unwrap();
    wait_count(cx, handle, 2).await;
    cx.run_until_parked();
    cx.background_executor
        .advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.click("find-duplicates", cx);
        window.click("cancel-duplicates", cx);
    })
    .unwrap();
    wait_count(cx, handle, 2).await;
    cx.update_window(handle, |_, window, cx| type_query(window, cx, "beta"))
        .unwrap();
    wait_count(cx, handle, 1).await;
    cx.update_window(handle, |_, window, cx| window.click("find-duplicates", cx))
        .unwrap();
    cx.wait_for(handle, Duration::from_secs(5), |window, _| {
        window.try_find("duplicate-summary").is_some()
    })
    .await;
    wait_count(cx, handle, 0).await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.find("duplicate-summary").label(),
            Some("0 duplicate groups, 0 files, 0 skipped")
        );
        window.click("close-duplicates", cx);
    })
    .unwrap();
    wait_count(cx, handle, 1).await;
}

#[gpui_kit::test]
async fn date_filters_and_automatic_typo_tolerance_work_in_the_search_ui(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let yesterday = chrono::Local::now()
        .date_naive()
        .pred_opt()
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    use chrono::TimeZone;
    let yesterday = std::time::SystemTime::from(
        chrono::Local
            .from_local_datetime(&yesterday)
            .earliest()
            .unwrap(),
    );
    fs::File::open(fixture.root.join("beta.JPG"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(yesterday))
        .unwrap();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| {
        type_query(window, cx, "file: modified:today")
    })
    .unwrap();
    wait_count(cx, handle, 3).await;
    cx.update_window(handle, |_, window, cx| {
        type_query(window, cx, "dm:yesterday")
    })
    .unwrap();
    wait_count(cx, handle, 1).await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.find(("result", 0usize)).label(),
            fixture.root.join("beta.JPG").to_str()
        );
        type_query(window, cx, "alpa");
    })
    .unwrap();
    wait_count(cx, handle, 2).await;
    cx.update_window(handle, |_, window, cx| {
        assert!(window.try_find("fuzzy").is_none());
        window.click("regex", cx);
        assert!(window.try_find("fuzzy").is_none());
    })
    .unwrap();
    wait_count(cx, handle, 0).await;
    cx.update_window(handle, |_, window, cx| {
        window.click("regex", cx);
        type_query(window, cx, "\"alpa\"");
    })
    .unwrap();
    wait_count(cx, handle, 0).await;
}

#[gpui_kit::test]
async fn folder_scope_and_saved_searches_restore_options_after_reopening(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    // A similarly prefixed sibling must stay outside the chosen folder.
    fs::create_dir(fixture.root.join("Archive2")).unwrap();
    fs::write(fixture.root.join("Archive2/alpha-other.txt"), b"fixture").unwrap();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 8).await;
    cx.update_window(handle, |_, window, cx| {
        window.right_click(("result", 1usize), cx)
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("popup-menu").is_some()
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.within("popup-menu").find(9usize).label(),
            Some("Search here")
        );
        window.within("popup-menu").click(9usize, cx);
    })
    .unwrap();
    wait_count(cx, handle, 1).await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.find("search-scope").label(),
            Some(
                format!(
                    "Searching within {}",
                    fixture.root.join("Archive").display()
                )
                .as_str()
            )
        );
        assert_eq!(window.find("search-input").value(), Some(""));
        window.click("case", cx);
        window.click(("filter", 6usize), cx);
        type_query(window, cx, "alpa");
        window.click("Modified", cx);
        window.click("Modified", cx);
    })
    .unwrap();
    wait_count(cx, handle, 1).await;
    cx.update_window(handle, |_, window, cx| {
        window.click("saved-searches-toggle", cx);
        window.click("saved-search-name", cx);
        window.input("Archived", cx);
        window.press("space", cx);
        assert_eq!(window.find("saved-search-name").focused(), Some(true));
        assert_eq!(window.find("saved-search-name").value(), Some("Archived "));
        assert_eq!(window.find("search-input").value(), Some("alpa"));
        window.input("documents", cx);
        window.click("save-search", cx);
        assert!(window.try_find("saved-searches").is_none());
        assert_eq!(window.find("search-input").focused(), Some(true));
        window.click("clear-scope", cx);
        assert!(window.try_find("search-scope").is_none());
    })
    .unwrap();
    wait_count(cx, handle, 3).await;
    let mut persisted: Vec<crate::saved_searches::SavedSearch> =
        crate::settings::read_json(&fixture.directory.join("cache/saved-searches.json")).unwrap();
    assert_eq!(persisted.len(), 1);
    assert!(persisted[0].descending);
    assert_eq!(persisted[0].sort, crate::saved_searches::Sort::Modified);
    assert_eq!(persisted[0].filter, 6);
    persisted[0].fuzzy = false;
    crate::settings::write_json(
        &fixture.directory.join("cache/saved-searches.json"),
        &persisted,
    )
    .unwrap();
    let reopened = fixture.open(cx);
    wait_count(cx, reopened, 8).await;
    cx.update_window(reopened, |_, window, cx| {
        window.click("saved-searches-toggle", cx);
        assert_eq!(
            window.find(("saved-search", 0usize)).label(),
            Some("Archived documents")
        );
        window.click(("saved-search", 0usize), cx);
        assert!(window.try_find("saved-searches").is_none());
        assert_eq!(window.find("search-input").value(), Some("alpa"));
        assert_eq!(window.find("search-input").focused(), Some(true));
        assert!(window.try_find("fuzzy").is_none());
        assert_eq!(window.find("case").checked(), Some(true));
        assert_eq!(window.find(("filter", 6usize)).checked(), Some(true));
        assert!(window.find("search-scope").visible());
    })
    .unwrap();
    wait_count(cx, reopened, 1).await;
    cx.update_window(reopened, |_, window, cx| {
        window.click("saved-searches-toggle", cx);
        window.click(("remove-saved-search", 0usize), cx);
        assert!(window.try_find(("saved-search", 0usize)).is_none());
        window.press("escape", cx);
    })
    .unwrap();
    let persisted: Vec<crate::saved_searches::SavedSearch> =
        crate::settings::read_json(&fixture.directory.join("cache/saved-searches.json")).unwrap();
    assert!(persisted.is_empty());
}

#[gpui_kit::test]
async fn dragging_a_selected_row_preserves_the_selection(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| {
        window.click(("result", 1usize), cx);
        window.click_with_modifiers(
            ("result", 3usize),
            gpui_kit::Modifiers {
                platform: true,
                ..Default::default()
            },
            cx,
        );
        let from = window.find(("result", 1usize)).bounds().center();
        window.drag(from, gpui_kit::point(px(-10.), from.y), cx);
        assert_eq!(window.find(("result", 1usize)).selected(), Some(true));
        assert_eq!(window.find(("result", 3usize)).selected(), Some(true));
    })
    .unwrap();
    assert!(fixture.root.join("Archive/alpha-old.txt").is_file());
    assert!(fixture.root.join("alpha.txt").is_file());
    // A normal click still collapses a multiple selection after releasing the mouse.
    cx.update_window(handle, |_, window, cx| {
        window.click(("result", 3usize), cx);
        assert_eq!(window.find(("result", 1usize)).selected(), Some(false));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn custom_title_bar_keeps_search_and_settings_accessible(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let titlebar = window.find("title-bar").bounds();
        assert_eq!(titlebar.size.height, px(40.));
        assert!(titlebar.contains(&window.find("app-title").bounds().center()));
        assert!(titlebar.contains(&window.find("titlebar-settings").bounds().center()));
        let duplicates = window.find("find-duplicates").bounds();
        let settings = window.find("titlebar-settings").bounds();
        assert!(titlebar.contains(&duplicates.center()));
        assert!(duplicates.right() < settings.left());
        assert_eq!(duplicates.size, settings.size);
        assert_eq!(duplicates.center().y, settings.center().y);
        assert_eq!(
            window.find("find-duplicates").label(),
            Some("Find duplicate files")
        );
        assert!(window.find("search-input").bounds().top() >= titlebar.bottom());
        assert!(window.find("result-count").visible());
        assert!(window.try_find("preview-badge").is_none());
        window.click("titlebar-settings", cx);
    })
    .unwrap();
    wait_dialog(cx, handle, true).await;
    cx.update_window(handle, |_, window, cx| {
        window.within("dialog").click("cancel", cx);
    })
    .unwrap();
    wait_dialog(cx, handle, false).await;
    cx.update_window(handle, |_, window, cx| {
        window.press("cmd-f", cx);
        window.input("alpha", cx);
        assert_eq!(window.find("search-input").value(), Some("alpha"));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn search_history_recalls_removes_persists_and_dismisses(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| {
        window.click("search-history-toggle", cx);
        assert!(window.find("search-history").visible());
        assert!(window.try_find(("search-history-query", 0usize)).is_none());
        window.press("escape", cx);
        assert!(window.try_find("search-history").is_none());
        assert_eq!(window.find("search-input").focused(), Some(true));
        type_query(window, cx, "alp");
        type_query(window, cx, "alpha");
    })
    .unwrap();
    wait_count(cx, handle, 2).await;
    cx.update_window(handle, |_, window, cx| {
        window.click("search-history-toggle", cx);
        assert_eq!(
            window.find(("search-history-query", 0usize)).label(),
            Some("alpha")
        );
        assert!(window.try_find(("search-history-query", 1usize)).is_none());
        let panel = window.find("search-history").bounds();
        let input = window.find("search-input").bounds();
        assert!(panel.origin.y >= input.bottom());
        assert!(panel.right() <= input.right());
        window.press("escape", cx);
        type_query(window, cx, "beta");
    })
    .unwrap();
    wait_count(cx, handle, 1).await;
    cx.update_window(handle, |_, window, cx| {
        window.click("search-history-toggle", cx);
        assert_eq!(
            window.find(("search-history-query", 0usize)).label(),
            Some("beta")
        );
        assert_eq!(
            window.find(("search-history-query", 1usize)).label(),
            Some("alpha")
        );
        window.click(("search-history-query", 1usize), cx);
        assert!(window.try_find("search-history").is_none());
        assert_eq!(window.find("search-input").value(), Some("alpha"));
        assert_eq!(window.find("search-input").focused(), Some(true));
    })
    .unwrap();
    wait_count(cx, handle, 2).await;
    cx.update_window(handle, |_, window, cx| {
        window.click("search-history-toggle", cx);
        assert_eq!(
            window.find(("search-history-query", 0usize)).label(),
            Some("alpha")
        );
        window.click(("search-history-remove", 0usize), cx);
        assert_eq!(
            window.find(("search-history-query", 0usize)).label(),
            Some("beta")
        );
        assert_eq!(window.find("search-input").value(), Some("alpha"));
        window.press("escape", cx);
        window.click("search-history-toggle", cx);
        assert_eq!(
            window.find(("search-history-query", 0usize)).label(),
            Some("beta")
        );
        assert!(window.try_find(("search-history-query", 1usize)).is_none());
        window.click(("result", 0usize), cx);
        assert!(window.try_find("search-history").is_none());
    })
    .unwrap();
    let reopened = fixture.open(cx);
    wait_count(cx, reopened, 6).await;
    cx.update_window(reopened, |_, window, cx| {
        window.click("search-history-toggle", cx);
        assert_eq!(
            window.find(("search-history-query", 0usize)).label(),
            Some("beta")
        );
        window.press("tab", cx);
        assert_eq!(
            window.find(("search-history-query", 0usize)).focused(),
            Some(true)
        );
        window.press("enter", cx);
        assert_eq!(window.find("search-input").value(), Some("beta"));
        assert!(window.try_find("search-history").is_none());
    })
    .unwrap();
    wait_count(cx, reopened, 1).await;
    cx.update_window(reopened, |_, window, cx| {
        type_query(window, cx, "alpha |");
        window.click("search-history-toggle", cx);
        assert_eq!(
            window.find(("search-history-query", 0usize)).label(),
            Some("beta")
        );
        assert!(window.try_find(("search-history-query", 1usize)).is_none());
        window.press("tab", cx);
        window.press("tab", cx);
        assert_eq!(
            window.find(("search-history-remove", 0usize)).focused(),
            Some(true)
        );
        window.press("enter", cx);
        assert!(window.find("search-history").visible());
        assert!(window.try_find(("search-history-query", 0usize)).is_none());
    })
    .unwrap();
    let saved: Vec<String> =
        crate::settings::read_json(&fixture.directory.join("cache/search-history.json")).unwrap();
    assert!(saved.is_empty());
    cx.update_window(reopened, |_, window, cx| {
        window.press("escape", cx);
        type_query(window, cx, &"averylongquery".repeat(12));
        window.click("search-history-toggle", cx);
        let panel = window.find("search-history").bounds();
        let query = window.find(("search-history-query", 0usize)).bounds();
        let remove = window.find(("search-history-remove", 0usize)).bounds();
        assert!(panel.size.width <= px(480.));
        assert!(query.right() <= remove.origin.x);
        assert!(remove.right() <= panel.right());
    })
    .unwrap();
}

#[gpui_kit::test]
async fn search_history_saves_only_the_latest_query_after_typing_pauses(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| {
        type_query(window, cx, "alp");
        type_query(window, cx, "alpha");
    })
    .unwrap();
    cx.run_until_parked();
    cx.background_executor.advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let saved: Vec<String> =
        crate::settings::read_json(&fixture.directory.join("cache/search-history.json")).unwrap();
    assert_eq!(saved, ["alpha"]);
    cx.update_window(handle, |_, window, _| {
        assert_eq!(window.find("search-input").focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn selecting_results_blurs_search_and_space_only_previews_results(cx: &mut TestAppContext) {
    use std::{cell::Cell, rc::Rc};

    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    let previews = Rc::new(Cell::new(0));
    let observed = previews.clone();
    let _subscription = cx.update(|cx| {
        cx.intercept_keystrokes(move |event, window, cx| {
            if event.keystroke.key == "space"
                && window.focused(cx).is_some_and(|focus| {
                    !window
                        .bindings_for_action_in(&search_view::QuickLook, &focus)
                        .is_empty()
                })
            {
                observed.set(observed.get() + 1);
                cx.stop_propagation();
            }
        })
    });
    cx.update_window(handle, |_, window, cx| {
        type_query(window, cx, "alpha");
        window.press("space", cx);
        assert_eq!(window.find("search-input").value(), Some("alpha "));
        assert_eq!(previews.get(), 0);
    })
    .unwrap();
    wait_count(cx, handle, 2).await;
    cx.update_window(handle, |_, window, cx| {
        window.click(("result", 0usize), cx);
        assert_eq!(window.find("search-input").focused(), Some(false));
        assert_eq!(window.find("results-table").focused(), Some(true));
        window.press("space", cx);
        assert_eq!(previews.get(), 1);
        assert_eq!(window.find("search-input").value(), Some("alpha "));
        window.press("down", cx);
        assert_eq!(window.find(("result", 1usize)).selected(), Some(true));
        window.press("down", cx);
        assert_eq!(window.find(("result", 1usize)).selected(), Some(true));
        window.press("up", cx);
        assert_eq!(window.find(("result", 0usize)).selected(), Some(true));
        window.press("up", cx);
        assert_eq!(window.find(("result", 0usize)).selected(), Some(true));
        window.press("space", cx);
        assert_eq!(previews.get(), 2);
        window.press("cmd-f", cx);
        assert_eq!(window.find("search-input").focused(), Some(true));
        window.press("space", cx);
        assert_eq!(window.find("search-input").value(), Some("alpha  "));
        assert_eq!(previews.get(), 2);
        window.press("down", cx);
        assert_eq!(window.find("search-input").focused(), Some(false));
        window.press("space", cx);
        assert_eq!(previews.get(), 3);
    })
    .unwrap();
}

#[gpui_kit::test]
async fn result_context_menu_selects_clicked_row_copies_path_and_dismisses(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    for row in [1usize, 0usize] {
        cx.update_window(handle, |_, window, cx| {
            window.right_click(("result", row), cx)
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(3), |window, _| {
            window.try_find("popup-menu").is_some()
        })
        .await;
        let expected = cx
            .update_window(handle, |_, window, cx| {
                assert_eq!(window.find(("result", row)).selected(), Some(true));
                let expected = window.find(("result", row)).label().unwrap().to_owned();
                let menu = window.within("popup-menu");
                assert_eq!(menu.find(0usize).label(), Some("Open"));
                assert_eq!(menu.find(1usize).label(), Some("Show in Finder"));
                assert_eq!(menu.find(2usize).label(), Some("Get Info"));
                assert_eq!(menu.find(4usize).label(), Some("Copy"));
                assert_eq!(menu.find(5usize).label(), Some("Copy path"));
                assert_eq!(menu.find(6usize).label(), Some("Rename…"));
                assert_eq!(menu.find(8usize).label(), Some("Move to Trash"));
                window.within("popup-menu").click(5usize, cx);
                expected
            })
            .unwrap();
        cx.wait_for(handle, Duration::from_secs(3), |window, cx| {
            window.try_find("popup-menu").is_none()
                && cx.read_from_clipboard().and_then(|item| item.text()) == Some(expected.clone())
        })
        .await;
    }
    cx.update_window(handle, |_, window, cx| {
        window.right_click(("result", 1usize), cx)
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("popup-menu").is_some()
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        window.press("escape", cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("popup-menu").is_none()
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(window.find("search-input").focused(), Some(false));
        assert_eq!(window.find("results-table").focused(), Some(true));
        assert!(
            !window
                .bindings_for_action_in(&search_view::QuickLook, &window.focused(cx).unwrap())
                .is_empty()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn trash_menu_reports_missing_file_without_touching_other_results(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| {
        window.right_click(("result", 3usize), cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("popup-menu").is_some()
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.find(("result", 3usize)).label(),
            fixture.root.join("alpha.txt").to_str()
        );
        fs::remove_file(fixture.root.join("alpha.txt")).unwrap();
        window.within("popup-menu").click(8usize, cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window
            .try_find("search-error")
            .and_then(|error| error.label().map(str::to_owned))
            .is_some_and(|label| label.starts_with("Couldn’t move to Trash:"))
    })
    .await;
    assert_eq!(fs::read(fixture.root.join("beta.JPG")).unwrap(), b"fixture");
}

#[gpui_kit::test]
async fn rename_menu_can_cancel_reject_collisions_and_update_results(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    for cancel in [true, false] {
        cx.update_window(handle, |_, window, cx| {
            window.right_click(("result", 3usize), cx);
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(3), |window, _| {
            window.try_find("popup-menu").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window.within("popup-menu").click(6usize, cx);
        })
        .unwrap();
        wait_dialog(cx, handle, true).await;
        cx.update_window(handle, |_, window, cx| {
            assert_eq!(window.find("rename-input").value(), Some("alpha.txt"));
            window.click("rename-input", cx);
            window.press("cmd-a", cx);
            window.input("beta.JPG", cx);
            if cancel {
                window.within("dialog").click("cancel", cx);
            } else {
                window.within("dialog").click("rename", cx);
            }
        })
        .unwrap();
        if cancel {
            wait_dialog(cx, handle, false).await;
        } else {
            cx.wait_for(handle, Duration::from_secs(3), |window, _| {
                window.try_find("rename-error").is_some()
            })
            .await;
            assert_eq!(
                fs::read(fixture.root.join("alpha.txt")).unwrap(),
                b"fixture"
            );
            assert_eq!(fs::read(fixture.root.join("beta.JPG")).unwrap(), b"fixture");
            cx.update_window(handle, |_, window, cx| {
                window.click("rename-input", cx);
                window.press("cmd-a", cx);
                window.input("renamed 雪.txt", cx);
                window.press("enter", cx);
            })
            .unwrap();
            wait_dialog(cx, handle, false).await;
        }
    }
    assert!(!fixture.root.join("alpha.txt").exists());
    assert_eq!(
        fs::read(fixture.root.join("renamed 雪.txt")).unwrap(),
        b"fixture"
    );
    cx.wait_for(handle, Duration::from_secs(10), |window, _| {
        std::thread::sleep(Duration::from_millis(2));
        (0..6usize).any(|row| {
            window.try_find(("result", row)).is_some_and(|result| {
                result.label() == fixture.root.join("renamed 雪.txt").to_str()
            })
        })
    })
    .await;
}

#[gpui_kit::test]
async fn help_text_can_be_selected_and_copied_but_not_edited(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| window.click("help", cx))
        .unwrap();
    wait_dialog(cx, handle, true).await;
    cx.update_window(handle, |_, window, cx| {
        let top = window.find("dialog-message").bounds().top();
        window.scroll(
            "dialog",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-300.))),
            cx,
        );
        assert!(
            window.find("dialog-message").bounds().top() < top,
            "Long help text must scroll inside the dialog"
        );
        window.scroll(
            "dialog",
            gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(10000.))),
            cx,
        );
        let bounds = window.find("dialog-message").bounds();
        let start = bounds.origin + gpui_kit::point(px(0.1), px(10.));
        let end = gpui_kit::point(bounds.right() - px(0.1), bounds.bottom() - px(0.1));
        window.drag(start, end, cx);
        window.press("cmd-c", cx);
        let message = cx.read_from_clipboard().unwrap().text().unwrap();
        assert!(message.starts_with("Search filenames as you type."));
        assert!(message.contains("*.pdf | *.docx"));
        assert!(message.contains("<cat | dog> photo"));
        assert!(message.contains("budget — both words\n\"annual report\" — exact phrase\n"));
        assert!(message.contains("extension filter\nfile: / folder: — kind\n"));
        assert!(message.contains("modified:>=2026-10-01 — changed on or after October 1, 2026"));
        assert!(message.contains("⌘, Folders"));
        window.input("replacement", cx);
        window.drag(start, end, cx);
        window.press("cmd-c", cx);
        assert_eq!(cx.read_from_clipboard().unwrap().text(), Some(message));
        let origin = window.find("dialog-message").bounds().origin;
        window.drag(
            origin + gpui_kit::point(px(8.), px(10.)),
            origin + gpui_kit::point(px(150.), px(10.)),
            cx,
        );
        window.press("cmd-c", cx);
        let selected = cx.read_from_clipboard().unwrap().text().unwrap();
        assert!(!selected.is_empty());
        assert!("Search filenames as you type.".contains(selected.trim()));
        window.press("escape", cx);
    })
    .unwrap();
    wait_dialog(cx, handle, false).await;
}

#[gpui_kit::test]
async fn search_filters_invalid_queries_latest_input_and_keyboard_selection(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| window.click(("filter", 1usize), cx))
        .unwrap();
    wait_count(cx, handle, 4).await;
    cx.update_window(handle, |_, window, cx| window.click(("filter", 4usize), cx))
        .unwrap();
    wait_count(cx, handle, 1).await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.find(("result", 0usize)).label(),
            fixture.root.join("beta.JPG").to_str()
        );
        window.click(("filter", 0usize), cx);
        type_query(window, cx, "alpha");
    })
    .unwrap();
    wait_count(cx, handle, 2).await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(window.find("result-count").label(), Some("2 matches"));
        let summary = window.find("result-count").bounds();
        let table = window.find("results-table").bounds();
        assert!((summary.center().x - table.center().x).abs() < px(1.));
        assert!(summary.origin.y >= table.bottom());
        assert_eq!(window.find(("result", 0usize)).selected(), Some(true));
        window.press("down", cx);
        assert_eq!(window.find(("result", 1usize)).selected(), Some(true));
        window.press("up", cx);
        assert_eq!(window.find(("result", 0usize)).selected(), Some(true));
        type_query(window, cx, "alpha |");
    })
    .unwrap();
    wait_count(cx, handle, 0).await;
    cx.update_window(handle, |_, window, cx| {
        assert!(window.try_find(("result", 0usize)).is_none());
        type_query(window, cx, "alpha");
        type_query(window, cx, "beta");
    })
    .unwrap();
    wait_count(cx, handle, 1).await;
    cx.update_window(handle, |_, window, _| {
        assert_eq!(window.find("search-input").value(), Some("beta"));
        assert_eq!(
            window.find(("result", 0usize)).label(),
            fixture.root.join("beta.JPG").to_str()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn content_mode_is_opt_in_and_restores_filename_controls(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| {
        let options = window.find("search-options");
        assert_eq!(options.label(), Some("Search options"));
        let bounds = options.bounds();
        for (id, label) in [
            ("content", "Search file contents"),
            ("case", "Case sensitive"),
            ("path", "Search full paths"),
            ("regex", "Use regular expressions"),
        ] {
            let control = window.find(id);
            assert_eq!(control.label(), Some(label));
            assert!(bounds.contains(&control.bounds().origin));
            assert!(bounds.contains(&control.bounds().bottom_right()));
        }
        assert_ne!(window.find("content").checked(), Some(true));
        window.click("path", cx);
        window.click("regex", cx);
        window.click("content", cx);
        assert_eq!(window.find("content").checked(), Some(true));
        assert_ne!(window.find("case").disabled(), Some(true));
        window.click("case", cx);
        assert_eq!(window.find("case").checked(), Some(true));
        window.click("path", cx);
        window.click("regex", cx);
        assert_eq!(window.find("path").checked(), Some(true));
        assert_eq!(window.find("regex").checked(), Some(true));
        assert_eq!(
            window.find("search-input").label(),
            Some("Search file contents")
        );
        assert!(window.find("content-search-note").visible());
    })
    .unwrap();
    wait_count(cx, handle, 0).await;
    cx.update_window(handle, |_, window, cx| {
        window.click("search-input", cx);
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("x".repeat(4097)));
        window.press("cmd-v", cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("search-error").is_some()
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        assert!(
            window
                .find("search-error")
                .label()
                .unwrap()
                .contains("4096")
        );
        type_query(window, cx, "alpha.*");
        window.click("content", cx);
        assert_ne!(window.find("path").disabled(), Some(true));
        assert_ne!(window.find("regex").disabled(), Some(true));
        assert_eq!(window.find("path").checked(), Some(true));
        assert_eq!(window.find("regex").checked(), Some(true));
        assert!(window.try_find("content-search-note").is_none());
    })
    .unwrap();
    wait_count(cx, handle, 2).await;
    cx.update_window(handle, |_, window, cx| {
        window.click("content", cx);
        type_query(window, cx, "");
    })
    .unwrap();
    wait_count(cx, handle, 0).await;
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("reset-search").is_some()
    })
    .await;
    cx.update_window(handle, |_, window, cx| window.click("reset-search", cx))
        .unwrap();
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, _| {
        assert_ne!(window.find("content").checked(), Some(true));
        assert_ne!(window.find("path").checked(), Some(true));
        assert_ne!(window.find("regex").checked(), Some(true));
        assert!(window.try_find("search-error").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
async fn indexed_folder_changes_cancel_or_persist_on_save(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("cmd-,", cx);
    })
    .unwrap();
    wait_dialog(cx, handle, true).await;
    cx.update_window(handle, |_, window, cx| {
        window.within("dialog").click(("remove-folder", 0usize), cx);
        window.within("dialog").click("cancel", cx);
    })
    .unwrap();
    wait_dialog(cx, handle, false).await;
    assert_eq!(
        Settings::read(&fixture.settings).unwrap().roots,
        vec![fixture.root.clone()]
    );
    cx.update(|cx| assert_eq!(cx.global::<Settings>().roots, vec![fixture.root.clone()]));
    cx.update_window(handle, |_, window, cx| window.press("cmd-,", cx))
        .unwrap();
    wait_dialog(cx, handle, true).await;
    cx.update_window(handle, |_, window, cx| {
        assert!(
            window
                .within("dialog")
                .try_find(("remove-folder", 0usize))
                .is_some()
        );
        window.within("dialog").click(("remove-folder", 0usize), cx);
        window.within("dialog").click("save", cx);
    })
    .unwrap();
    wait_dialog(cx, handle, false).await;
    assert!(Settings::read(&fixture.settings).unwrap().roots.is_empty());
    cx.update(|cx| assert!(cx.global::<Settings>().roots.is_empty()));
    wait_count(cx, handle, 0).await;
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("empty-add-folders").is_some()
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        assert!(window.try_find("empty-add-folders").is_some());
        window.click("empty-add-folders", cx);
    })
    .unwrap();
    wait_dialog(cx, handle, true).await;
    cx.update_window(handle, |_, window, cx| {
        assert!(
            window
                .within("dialog")
                .try_find("no-indexed-folders")
                .is_some()
        );
        window.within("dialog").click("cancel", cx);
    })
    .unwrap();
    wait_dialog(cx, handle, false).await;
}

#[gpui_kit::test]
async fn empty_search_can_reset_all_filters(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| {
        window.click(("filter", 4usize), cx);
        window.click("case", cx);
        window.click("path", cx);
        window.click("regex", cx);
        type_query(window, cx, "does-not-exist");
    })
    .unwrap();
    wait_count(cx, handle, 0).await;
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("reset-search").is_some()
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        window.click("reset-search", cx);
    })
    .unwrap();
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(window.find("search-input").value(), Some(""));
        assert!(window.try_find("reset-search").is_none());
        type_query(window, cx, "ALPHA !old");
    })
    .unwrap();
    wait_count(cx, handle, 1).await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(
            window.find(("result", 0usize)).label(),
            fixture.root.join("alpha.txt").to_str()
        );
        type_query(
            window,
            cx,
            fixture.directory.file_name().unwrap().to_str().unwrap(),
        );
    })
    .unwrap();
    wait_count(cx, handle, 0).await;
}

#[gpui_kit::test]
async fn multiple_selection_uses_modifiers_keyboard_and_preserves_context_selection(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| {
        window.click(("result", 1usize), cx);
        window.click_with_modifiers(
            ("result", 3usize),
            gpui_kit::Modifiers {
                platform: true,
                ..Default::default()
            },
            cx,
        );
        assert_eq!(window.find(("result", 1usize)).selected(), Some(true));
        assert_eq!(window.find(("result", 3usize)).selected(), Some(true));
        window.click("copy-path", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            format!(
                "{}\n{}",
                fixture.root.join("Archive").display(),
                fixture.root.join("alpha.txt").display()
            )
        );
        window.right_click(("result", 1usize), cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("popup-menu").is_some()
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(window.find(("result", 3usize)).selected(), Some(true));
        assert_eq!(
            window.within("popup-menu").find(5usize).label(),
            Some("Copy paths")
        );
        window.within("popup-menu").click(6usize, cx);
        assert!(
            window.try_find("dialog").is_none(),
            "Multiple selection cannot rename one arbitrary result"
        );
        window.press("escape", cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(3), |window, _| {
        window.try_find("popup-menu").is_none()
    })
    .await;
    cx.update_window(handle, |_, window, cx| {
        window.click(("result", 1usize), cx);
        window.click_with_modifiers(
            ("result", 4usize),
            gpui_kit::Modifiers {
                shift: true,
                ..Default::default()
            },
            cx,
        );
        for i in 1usize..=4 {
            assert_eq!(window.find(("result", i)).selected(), Some(true));
        }
        window.click_with_modifiers(
            ("result", 2usize),
            gpui_kit::Modifiers {
                shift: true,
                ..Default::default()
            },
            cx,
        );
        assert_eq!(window.find(("result", 3usize)).selected(), Some(false));
        window.press("shift-down", cx);
        assert_eq!(window.find(("result", 3usize)).selected(), Some(true));
        window.press("shift-up", cx);
        assert_eq!(window.find(("result", 3usize)).selected(), Some(false));
        window.press("cmd-a", cx);
        for i in 0usize..6 {
            assert_eq!(window.find(("result", i)).selected(), Some(true));
        }
        window.press("shift-cmd-c", cx);
        assert_eq!(
            cx.read_from_clipboard()
                .unwrap()
                .text()
                .unwrap()
                .lines()
                .count(),
            6
        );
        window.click(("result", 3usize), cx);
        window.click_with_modifiers(
            ("result", 4usize),
            gpui_kit::Modifiers {
                platform: true,
                ..Default::default()
            },
            cx,
        );
        window.press("cmd-c", cx);
        window.click_with_modifiers(
            ("result", 3usize),
            gpui_kit::Modifiers {
                platform: true,
                ..Default::default()
            },
            cx,
        );
        assert_eq!(window.find(("result", 3usize)).selected(), Some(false));
        window.click_with_modifiers(
            ("result", 4usize),
            gpui_kit::Modifiers {
                platform: true,
                ..Default::default()
            },
            cx,
        );
        for i in 0usize..6 {
            assert_eq!(window.find(("result", i)).selected(), Some(false));
        }
        window.click("search-input", cx);
        window.input("alpha", cx);
        window.press("cmd-a", cx);
        window.input("beta", cx);
        assert_eq!(window.find("search-input").value(), Some("beta"));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn excluded_folders_can_be_cancelled_saved_and_removed(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    let excluded = fixture.root.join("Archive");
    for save in [false, true] {
        cx.update_window(handle, |_, window, cx| window.press("cmd-,", cx))
            .unwrap();
        wait_dialog(cx, handle, true).await;
        cx.update_window(handle, |_, window, cx| {
            window.within("dialog").click("exclude-folder", cx)
        })
        .unwrap();
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|options| {
            assert!(options.directories && !options.files && options.multiple);
            Some(vec![excluded.clone(), excluded.clone()])
        });
        let remove_id = format!("remove-excluded-{}", excluded.display());
        cx.wait_for(handle, Duration::from_secs(3), |window, _| {
            window
                .within("dialog")
                .try_find(remove_id.clone())
                .is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            window
                .within("dialog")
                .click(if save { "save" } else { "cancel" }, cx)
        })
        .unwrap();
        wait_dialog(cx, handle, false).await;
        let settings = Settings::read(&fixture.settings).unwrap();
        assert_eq!(
            settings.excluded,
            if save {
                vec![excluded.clone()]
            } else {
                Vec::new()
            }
        );
    }
    wait_count(cx, handle, 4).await;
    assert!(excluded.join("alpha-old.txt").exists());
    cx.update_window(handle, |_, window, cx| window.press("cmd-,", cx))
        .unwrap();
    wait_dialog(cx, handle, true).await;
    cx.update_window(handle, |_, window, cx| {
        window
            .within("dialog")
            .click(format!("remove-excluded-{}", excluded.display()), cx);
        window.within("dialog").click("save", cx);
    })
    .unwrap();
    wait_dialog(cx, handle, false).await;
    wait_count(cx, handle, 6).await;
    assert!(
        Settings::read(&fixture.settings)
            .unwrap()
            .excluded
            .is_empty()
    );
}
