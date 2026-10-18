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
            });
            cx.bind_keys([
                KeyBinding::new("down", search_view::NextResult, Some("Everything")),
                KeyBinding::new("up", search_view::PreviousResult, Some("Everything")),
                KeyBinding::new("cmd-f", search_view::FocusSearch, Some("Everything")),
                KeyBinding::new("cmd-,", OpenSettings, None),
            ]);
            let (handle, view) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1150.), px(800.)),
                    })),
                    ..Default::default()
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
        window.find("results-table").label() == Some(label.as_str())
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
async fn help_text_can_be_selected_and_copied_but_not_edited(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let handle = fixture.open(cx);
    wait_count(cx, handle, 6).await;
    cx.update_window(handle, |_, window, cx| window.click("help", cx))
        .unwrap();
    wait_dialog(cx, handle, true).await;
    cx.update_window(handle, |_, window, cx| {
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
