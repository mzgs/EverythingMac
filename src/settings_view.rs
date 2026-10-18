use crate::settings::Settings;
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, WindowExt,
        button::{Button, ButtonVariants},
        dialog::{DialogAction, DialogFooter},
    },
    *,
};
use std::path::PathBuf;

pub(crate) fn register_action(
    handle: AnyWindowHandle,
    path: Option<PathBuf>,
    error: Option<String>,
    cx: &mut App,
) {
    cx.on_action(move |_: &crate::OpenSettings, cx| {
        let path = path.clone();
        let error = error.clone();
        cx.defer(move |cx| {
            if let Err(error) =
                cx.update_window(handle, |_, window, cx| open(path, error, window, cx))
            {
                eprintln!("Couldn’t open settings: {error}");
            }
        });
    });
}

pub(crate) fn show_error(message: String, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        dialog.title("Everything Mac").description(message.clone())
    });
}

fn open(path: Option<PathBuf>, error: Option<String>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let Some(path) = path else {
        show_error(
            error.unwrap_or_else(|| {
                "Settings saving is disabled. Fix settings.json and restart.".into()
            }),
            window,
            cx,
        );
        return;
    };
    let roots = cx.global::<Settings>().roots.clone();
    let form = cx.new(|_| FolderSettings {
        path,
        roots,
        error: None,
        busy: false,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let confirm = form.clone();
        dialog
            .title("Indexed folders")
            .width(px(650.))
            .child(form.clone())
            .on_ok(move |_, _, cx| {
                confirm.update(cx, |form, cx| {
                    if form.busy {
                        return false;
                    }
                    let settings = Settings {
                        roots: form.roots.clone(),
                    };
                    match settings.save(&form.path) {
                        Ok(()) => {
                            cx.set_global(settings);
                            cx.refresh_windows();
                            true
                        }
                        Err(error) => {
                            form.error = Some(format!("Couldn’t save folders: {error}"));
                            cx.notify();
                            false
                        }
                    }
                })
            })
    });
}

struct FolderSettings {
    path: PathBuf,
    roots: Vec<PathBuf>,
    error: Option<String>,
    busy: bool,
}

impl FolderSettings {
    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Index folders".into()),
        });
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = picker.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        for path in paths {
                            if !this.roots.contains(&path) {
                                this.roots.push(path);
                            }
                        }
                    }
                    Err(error) => {
                        this.error = Some(format!("Couldn’t choose folders: {error}"));
                    }
                    Ok(Err(error)) => {
                        this.error = Some(format!("Couldn’t choose folders: {error}"))
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for FolderSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().flex_col().gap_3()
            .child("Choose folders or volumes to search. Subfolders and hidden files are included. Symbolic links are listed without following them.")
            .child(div().id("folder-list").max_h(px(300.)).overflow_y_scroll().children(self.roots.iter().enumerate().map(|(index, path)| {
                div().flex().items_center().gap_2().py_1()
                    .child(div().flex_1().min_w_0().truncate().child(path.to_string_lossy().into_owned()))
                    .child(Button::new(("remove-folder", index)).label("Remove").on_click(cx.listener(move |this, _, _, cx| { this.roots.remove(index); cx.notify(); })))
            })))
            .child(Button::new("add-folder").label("Add folders…").disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.add(window, cx))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child("macOS may ask for folder access. For protected locations, grant Full Disk Access to Everything Mac in System Settings, then rebuild. Files remain on your Mac; only names and metadata are searched."))
            .children(self.error.clone().map(|error| div().id("settings-error").test_support().role(Role::Status).aria_label(error.clone()).text_color(cx.theme().danger).child(error)))
            .child(DialogFooter::new()
                .child(Button::new("cancel").label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                .child(DialogAction::new().child(Button::new("save").primary().label("Save folders").disabled(self.busy))))
    }
}
