mod bulk;
mod content_search;
mod duplicates;
mod file_actions;
mod fs_events;
mod index;
mod index_db;
mod query;
mod quick_look;
mod saved_searches;
mod search_history;
mod search_view;
mod settings;
mod settings_view;
#[cfg(test)]
mod ui_tests;
mod window_state;

use gpui_kit::component::{
    ActiveTheme, Sizable, Theme, TitleBar,
    button::{Button, ButtonVariants},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use settings::Settings;
use window_state::WindowState;

use gpui_kit::assets::AllAssets as Assets;

impl Global for Settings {}

actions!(base_app, [Quit, OpenSettings]);

struct AppView {
    app_icon: std::sync::Arc<Image>,
    search: Entity<search_view::SearchView>,
    window_path: Option<std::path::PathBuf>,
    window_state: WindowState,
}

impl AppView {
    fn new(
        window_path: Option<std::path::PathBuf>,
        restored: Option<WindowState>,
        database_path: Option<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut window_state = restored.unwrap_or_else(|| WindowState::capture(window));
        window_state.update(window);
        cx.observe_window_bounds(window, |view, window, _| view.window_state.update(window))
            .detach();
        cx.on_app_quit(|view, _| {
            view.save_window_state();
            async {}
        })
        .detach();
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = weak.update(cx, |view, _| {
                view.window_state.update(window);
                view.save_window_state();
            });
            true
        });
        let search = cx.new(|cx| search_view::SearchView::new(database_path, window, cx));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        Self {
            app_icon: Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../assets/AppIcon.png").to_vec(),
            )
            .into(),
            search,
            window_path,
            window_state,
        }
    }

    fn save_window_state(&self) {
        if let Some(path) = &self.window_path
            && let Err(error) = settings::write_json(path, &self.window_state)
        {
            eprintln!(
                "Couldn’t save window geometry to {}: {error}",
                path.display()
            );
        }
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let duplicates = self
            .search
            .update(cx, |search, cx| search.duplicates_button(cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new()
                    .h(px(40.))
                    .pl(px(96.))
                    .pr_5()
                    .border_color(cx.theme().link.opacity(0.16))
                    .bg(linear_gradient(
                        175.,
                        linear_color_stop(cx.theme().title_bar, 0.),
                        linear_color_stop(cx.theme().background, 1.),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(img(self.app_icon.clone()).size_7().flex_shrink_0())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("app-title")
                                            .test_support()
                                            .text_size(px(15.))
                                            .line_height(px(20.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("Everything Mac"),
                                    )
                                    .when(window.window_title().ends_with(" — Preview"), |this| {
                                        this.child(
                                            div()
                                                .id("preview-badge")
                                                .test_support()
                                                .px_1p5()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(cx.theme().link.opacity(0.2))
                                                .bg(cx.theme().selection)
                                                .text_size(px(10.))
                                                .line_height(px(16.))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(cx.theme().link)
                                                .child("PREVIEW"),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div().flex().items_center().gap_2().child(duplicates).child(
                            Button::new("titlebar-settings")
                                .ghost()
                                .small()
                                .size_7()
                                .rounded_lg()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().background.opacity(0.55))
                                .text_color(cx.theme().muted_foreground)
                                .icon(assets::IconName::Settings)
                                .accessibility_label("Settings")
                                .tooltip("Settings · ⌘,")
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(OpenSettings), cx)
                                }),
                        ),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.search.clone()))
    }
}

fn init_theme(cx: &mut App) {
    Theme::update(cx, |theme| {
        std::rc::Rc::make_mut(&mut theme.light_theme)
            .colors
            .selection = Some("#007aff33".into());
        let colors = &mut std::rc::Rc::make_mut(&mut theme.dark_theme).colors;
        colors.background = Some("#1c1e22".into());
        colors.foreground = Some("#e6e9ee".into());
        colors.title_bar = Some("#30343b".into());
        colors.title_bar_border = Some("#3d424a".into());
        colors.status_bar = Some("#24262b".into());
        colors.status_bar_border = Some("#3d424a".into());
        colors.border = Some("#3d424a".into());
        colors.input = Some("#626975".into());
        colors.muted = Some("#292c32".into());
        colors.muted_foreground = Some("#adb4bf".into());
        colors.accent = Some("#343941".into());
        colors.accent_foreground = Some("#e6e9ee".into());
        colors.primary = Some("#8bbcff".into());
        colors.primary_hover = Some("#a5ccff".into());
        colors.primary_active = Some("#73a9ed".into());
        colors.primary_foreground = Some("#172334".into());
        colors.secondary = Some("#343941".into());
        colors.secondary_hover = Some("#404650".into());
        colors.secondary_active = Some("#292c32".into());
        colors.secondary_foreground = Some("#e6e9ee".into());
        colors.link = Some("#8bbcff".into());
        colors.link_hover = Some("#a5ccff".into());
        colors.link_active = Some("#73a9ed".into());
        colors.ring = Some("#8bbcff".into());
        colors.selection = Some("#3979d14d".into());
        colors.table = Some("#1c1e22".into());
        colors.table_even = Some("#202328".into());
        colors.table_hover = Some("#2c3037".into());
        colors.table_active = Some("#4f91e933".into());
        colors.table_active_border = Some("#8bbcff".into());
        colors.table_head = Some("#292c32".into());
        colors.table_head_foreground = Some("#adb4bf".into());
        colors.table_row_border = Some("#3d424a".into());
        colors.popover = Some("#292c32".into());
        colors.popover_foreground = Some("#e6e9ee".into());
        colors.group_box = Some("#24262b".into());
        colors.group_box_foreground = Some("#e6e9ee".into());
        colors.progress_bar = Some("#8bbcff".into());
    });
    Theme::change(cx.theme().mode, None, cx);
}

fn main() {
    // shortcut: gpui-fast 0.1.6 scroll tiles overflow large/HiDPI lists; remove after an upstream fix.
    // SAFETY: This runs before any application or index worker threads start.
    unsafe { std::env::set_var("GPUI_SCROLL_LAYERS", "0") };
    let preview_path = if std::env::args().any(|arg| arg == "--preview") {
        let directory =
            std::env::temp_dir().join(format!("everything-mac-preview-{}", std::process::id()));
        match Settings::create_preview(&directory) {
            Ok(path) => Some(path),
            Err(error) => {
                eprintln!("Couldn’t create isolated UI preview: {error}");
                return;
            }
        }
    } else {
        None
    };
    let loaded = preview_path
        .clone()
        .map_or_else(Settings::path, Ok)
        .and_then(|path| {
            Settings::load(&path)
                .map(|settings| (path.clone(), settings))
                .map_err(|error| {
                    std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
                })
        });
    let (settings_path, settings, startup_error) = match loaded {
        Ok((path, settings)) => (Some(path), settings, None),
        Err(error) => (
            None,
            Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            },
            Some(format!(
                "Couldn’t load settings: {error}. Existing files are preserved and saving is disabled. Fix the file or folder permissions, then restart the app."
            )),
        ),
    };
    let (window_path, restored) = if preview_path.is_some() {
        (None, None)
    } else {
        match WindowState::load() {
            Ok(loaded) => loaded,
            Err(error) => {
                eprintln!(
                    "Couldn’t load window geometry: {error}. Existing file is preserved; geometry saving is disabled."
                );
                (None, None)
            }
        }
    };
    let window_handle = std::rc::Rc::new(std::cell::Cell::new(None::<AnyWindowHandle>));
    let application = gpui_kit::application().with_assets(Assets);
    application.on_reopen({
        let window_handle = window_handle.clone();
        move |cx| {
            if let Some(handle) = window_handle.get() {
                cx.defer(move |cx| {
                    if let Err(error) = cx.update_window(handle, |_, window, cx| {
                        window.activate_window();
                        cx.activate(true);
                    }) {
                        eprintln!("Couldn’t reactivate the app window: {error}");
                    }
                });
            }
        }
    });
    application.run(move |cx| {
        gpui_kit::init(cx);
        init_theme(cx);
        cx.set_global(settings);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let options = WindowOptions {
            window_bounds: Some(WindowState::window_bounds(restored.as_ref(), cx)),
            window_min_size: Some(size(px(1050.), px(450.))),
            titlebar: Some(TitlebarOptions {
                title: Some(
                    if preview_path.is_some() {
                        "Everything Mac — Preview"
                    } else {
                        "Everything Mac"
                    }
                    .into(),
                ),
                traffic_light_position: Some(point(px(16.), px(12.))),
                ..TitleBar::title_bar_options()
            }),
            ..TitleBar::window_options()
        };
        let database_path = settings_path
            .as_ref()
            .map(|path| path.with_file_name("index.sqlite3"));
        let (handle, view) = gpui_kit::open_window(options, cx, |window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
            window
                .observe_window_appearance(|window, cx| {
                    Theme::sync_system_appearance(Some(window), cx);
                })
                .detach();
            cx.new(|cx| AppView::new(window_path, restored, database_path, window, cx))
        })
        .expect("Failed to open app window");
        window_handle.set(Some(handle));
        cx.bind_keys([
            // Text fields inside popovers must receive spaces instead of toggling the popover.
            KeyBinding::new("space", NoAction, Some("Popover > Input")),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-f", search_view::FocusSearch, Some("Everything")),
            KeyBinding::new("cmd-r", search_view::RebuildIndex, Some("Everything")),
            KeyBinding::new("cmd-enter", search_view::RevealResult, Some("Everything")),
            KeyBinding::new("shift-cmd-c", search_view::CopyPath, Some("Everything")),
            KeyBinding::new("down", search_view::NextResult, Some("Everything")),
            KeyBinding::new("up", search_view::PreviousResult, Some("Everything")),
            KeyBinding::new("enter", search_view::OpenResult, Some("Everything")),
            KeyBinding::new("space", search_view::QuickLook, Some("FileResults")),
            KeyBinding::new("cmd-a", search_view::SelectAllResults, Some("FileResults")),
            KeyBinding::new("cmd-c", search_view::CopyFiles, Some("FileResults")),
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
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new("ctrl-q", Quit, None),
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new("ctrl-,", OpenSettings, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        settings_view::register_action(
            handle,
            view.read(cx).search.clone(),
            settings_path,
            startup_error.clone(),
            cx,
        );
        cx.set_menus(vec![Menu {
            name: "Everything Mac".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::action("Search help", search_view::SearchHelp),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
        }]);
        if let Some(error) = startup_error {
            let _ = cx.update_window(handle, |_, window, cx| {
                settings_view::show_error(error, window, cx);
            });
        }
        cx.activate(true);
    });
}
