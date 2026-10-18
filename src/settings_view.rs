use crate::settings::Settings;
use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, Disableable, WindowExt,
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        dialog::{DialogAction, DialogFooter},
        form::{Field, Form},
        input::{Input, InputState},
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
        // Action dispatch already borrows the window; open after it completes.
        cx.defer(move |cx| {
            if let Err(error) = cx.update_window(handle, |_, window, cx| {
                open(path, error, window, cx);
            }) {
                eprintln!("Couldn’t open settings: {error}");
            }
        });
    });
}

pub(crate) fn show_error(message: String, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |dialog, _, _| {
        dialog.title("Couldn’t load settings").description(
            div()
                .id("startup-error")
                .test_support()
                .role(Role::Status)
                .aria_label(message.clone())
                .child(message.clone()),
        )
    });
}

pub(crate) fn open(
    path: Option<PathBuf>,
    error: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    // A second shortcut invocation should keep the current draft.
    if window.has_active_dialog(cx) {
        return;
    }
    let Some(path) = path else {
        show_error(
            error.unwrap_or_else(|| {
                "Settings saving is disabled. Restart after fixing the settings file.".into()
            }),
            window,
            cx,
        );
        return;
    };
    let settings = cx.global::<Settings>().clone();
    let form = cx.new(|cx| SettingsForm::new(path, settings, window, cx));
    window.open_dialog(cx, move |dialog, window, _| {
        let confirm = form.clone();
        dialog
            .title("Settings")
            .width(window.rem_size() * 28.)
            .child(form.clone())
            .on_ok(move |_, _, cx| confirm.update(cx, |form, cx| form.save(cx)))
    });
}

struct SettingsForm {
    path: PathBuf,
    name: Entity<InputState>,
    limit: Entity<InputState>,
    enabled: bool,
    error: Option<String>,
    transfer_busy: bool,
    status: Option<String>,
}

impl SettingsForm {
    fn new(path: PathBuf, settings: Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Display name");
            state.set_value(settings.display_name, window, cx);
            state
        });
        let limit = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Recent items limit");
            state.set_value(settings.recent_items_limit.to_string(), window, cx);
            state
        });
        Self {
            path,
            name,
            limit,
            enabled: settings.notifications_enabled,
            error: None,
            transfer_busy: false,
            status: None,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) -> bool {
        if self.transfer_busy {
            return false;
        }
        let result = (|| -> Result<Settings, String> {
            let limit = self
                .limit
                .read(cx)
                .value()
                .trim()
                .parse::<u32>()
                .map_err(|_| {
                    "Enter a whole number from 0 to 4294967295 for the recent items limit."
                        .to_string()
                })?;
            let settings = Settings {
                display_name: self.name.read(cx).value().to_string(),
                notifications_enabled: self.enabled,
                recent_items_limit: limit,
            };
            settings.save(&self.path).map_err(|error| {
                format!(
                    "Couldn’t save {}: {error}. Your changes have not been saved.",
                    self.path.display()
                )
            })?;
            Ok(settings)
        })();
        match result {
            Ok(settings) => {
                cx.set_global(settings);
                cx.refresh_windows();
                true
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                false
            }
        }
    }

    fn transfer(&mut self, importing: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.transfer_busy {
            return;
        }
        let picker: Task<Result<Option<PathBuf>>> = if importing {
            let picker = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Import settings".into()),
            });
            cx.background_spawn(async move {
                Ok(picker.await??.and_then(|paths| paths.into_iter().next()))
            })
        } else {
            let directory = dirs::download_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(|| ".".into());
            let picker = cx.prompt_for_new_path(&directory, Some("settings.json"));
            cx.background_spawn(async move { picker.await? })
        };
        let saved = cx.global::<Settings>().clone();
        self.transfer_busy = true;
        self.error = None;
        self.status = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Option<Settings>> = async {
                let Some(path) = picker.await? else {
                    return Ok(None);
                };
                let settings = cx
                    .background_spawn(async move {
                        if importing {
                            Settings::read(&path)
                        } else {
                            saved.save(&path).map(|_| saved)
                        }
                    })
                    .await?;
                Ok(Some(settings))
            }
            .await;
            let _ = this.update_in(cx, |form, window, cx| {
                form.transfer_busy = false;
                match result {
                    Ok(Some(settings)) => {
                        if importing {
                            form.name.update(cx, |state, cx| {
                                state.set_value(settings.display_name, window, cx)
                            });
                            form.limit.update(cx, |state, cx| {
                                state.set_value(settings.recent_items_limit.to_string(), window, cx)
                            });
                            form.enabled = settings.notifications_enabled;
                        }
                        form.status = Some(
                            if importing {
                                "Settings imported. Review them and choose Save."
                            } else {
                                "Saved settings exported."
                            }
                            .into(),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        form.error = Some(format!(
                            "Couldn’t {} settings: {error}.",
                            if importing { "import" } else { "export" }
                        ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for SettingsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                Form::new()
                    .child(
                        Field::new()
                            .label("Display name")
                            .child(Input::new(&self.name).id("display-name")),
                    )
                    .child(
                        Field::new().label_indent(false).child(
                            Checkbox::new("notifications-enabled")
                                .label("Enable notifications")
                                .checked(self.enabled)
                                .on_change(cx.listener(|this, value, _, cx| {
                                    this.enabled = *value;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        Field::new()
                            .label("Recent items limit")
                            .child(Input::new(&self.limit).id("recent-items-limit")),
                    ),
            )
            .child(
                div()
                    .id("settings-transfer-status")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(self.status.clone().unwrap_or_default())
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .children(self.status.clone()),
            )
            .child(
                div()
                    .id("settings-error")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(self.error.clone().unwrap_or_default())
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .children(self.error.clone()),
            )
            .child(
                DialogFooter::new()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(DialogAction::new().child(Button::new("save").primary().label("Save"))),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_3()
                    .child(
                        Button::new("import-settings")
                            .outline()
                            .icon(IconName::Upload)
                            .label("Import settings…")
                            .disabled(self.transfer_busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.transfer(true, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("export-settings")
                            .outline()
                            .icon(IconName::Download)
                            .label("Export settings…")
                            .disabled(self.transfer_busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.transfer(false, window, cx)),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use crate::{AppView, Assets, OpenSettings, settings::Settings};
    use gpui_kit::{
        App, AppContext, AssetSource, KeyBinding, TestAppContext, Window,
        component::Root,
        px, size,
        test::{TestAppContextExt, TestWindowExt},
    };

    fn fill(window: &mut Window, id: &'static str, value: &str, cx: &mut App) {
        window.click(id, cx);
        window.press("secondary-a", cx);
        window.input(value, cx);
    }

    #[gpui_kit::test]
    async fn settings_dialog_validates_saves_cancels_and_reports_errors(cx: &mut TestAppContext) {
        for path in [
            "icons/upload.svg",
            "icons/download.svg",
            "icons/settings.svg",
        ] {
            assert!(!Assets.load(path).unwrap().unwrap().is_empty());
            assert!(
                Assets
                    .list("icons/")
                    .unwrap()
                    .iter()
                    .any(|name| name.as_ref() == path)
            );
        }
        let directory =
            std::env::temp_dir().join(format!("base-app-ui-test-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("settings.json");
        Settings::default().save(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Settings::default());
        });
        let handle = cx.open_window(size(px(800.), px(600.)), |window, cx| {
            let view = cx.new(|cx| AppView::new(None, None, window, cx));
            Root::new(view, window, cx)
        });
        cx.update(|cx| {
            super::register_action(handle.into(), Some(path.clone()), None, cx);
            cx.bind_keys([KeyBinding::new("cmd-,", OpenSettings, None)]);
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.press("cmd-,", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let import = window.find("import-settings");
            let export = window.find("export-settings");
            assert_eq!(import.bounds().top(), export.bounds().top());
            assert!(import.bounds().top() > window.find("save").bounds().bottom());
            fill(window, "display-name", "Discarded", cx);
            window.click("export-settings", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let backup = directory.join("backup.json");
        cx.simulate_new_path_selection(|_| Some(backup.clone()));
        cx.wait_for(
            handle.into(),
            std::time::Duration::from_secs(5),
            |window, _| {
                window
                    .find("settings-transfer-status")
                    .label()
                    .unwrap()
                    .contains("exported")
            },
        )
        .await;
        assert_eq!(Settings::read(&backup).unwrap(), Settings::default());
        std::fs::write(&backup, r#"{"recent_items_limit":-1}"#).unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("import-settings", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|options| {
            assert!(options.files && !options.directories && !options.multiple);
            Some(vec![backup.clone()])
        });
        cx.wait_for(
            handle.into(),
            std::time::Duration::from_secs(5),
            |window, _| {
                window
                    .find("settings-error")
                    .label()
                    .unwrap()
                    .contains("Couldn’t import")
            },
        )
        .await;
        cx.update_window(handle.into(), |_, window, _| {
            assert_eq!(window.find("display-name").value(), Some("Discarded"));
        })
        .unwrap();
        let imported = Settings {
            display_name: "Imported".into(),
            notifications_enabled: false,
            recent_items_limit: 5,
        };
        imported.save(&backup).unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("import-settings", cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|_| Some(vec![backup.clone()]));
        cx.wait_for(
            handle.into(),
            std::time::Duration::from_secs(5),
            |window, _| {
                window
                    .find("settings-transfer-status")
                    .label()
                    .unwrap()
                    .contains("imported")
            },
        )
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(window.find("display-name").value(), Some("Imported"));
            assert_eq!(window.find("recent-items-limit").value(), Some("5"));
            assert_eq!(window.find("notifications-enabled").checked(), Some(false));
            assert_eq!(cx.global::<Settings>(), &Settings::default());
            assert_eq!(std::fs::read(&path).unwrap(), original);
            window.click("import-settings", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.simulate_path_prompt_response(|_| None);
        cx.wait_for(
            handle.into(),
            std::time::Duration::from_secs(5),
            |window, _| window.find("import-settings").disabled() != Some(true),
        )
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(window.find("display-name").value(), Some("Imported"));
            window.press("escape", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        cx.update_window(handle.into(), |_, window, cx| {
            window.dispatch_action(Box::new(OpenSettings), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            fill(window, "display-name", "Ada", cx);
            window.click("notifications-enabled", cx);
            fill(window, "recent-items-limit", "-1", cx);
            window.click("save", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .find("settings-error")
                    .label()
                    .unwrap()
                    .contains("whole number")
            );
            assert_eq!(cx.global::<Settings>(), &Settings::default());
            fill(window, "recent-items-limit", "3", cx);
            std::fs::create_dir(path.with_extension(format!("{}.tmp", std::process::id())))
                .unwrap();
            window.click("save", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .find("settings-error")
                    .label()
                    .unwrap()
                    .contains("Couldn’t save")
            );
            assert_eq!(cx.global::<Settings>(), &Settings::default());
            assert_eq!(std::fs::read(&path).unwrap(), original);
            std::fs::remove_dir(path.with_extension(format!("{}.tmp", std::process::id())))
                .unwrap();
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let saved = Settings::load(&path).unwrap();
        assert_eq!(saved.display_name, "Ada");
        assert!(!saved.notifications_enabled);
        assert_eq!(saved.recent_items_limit, 3);
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("save").is_none());
            assert_eq!(cx.global::<Settings>(), &saved);
            super::open(
                None,
                Some("Fix settings.json, then restart.".into()),
                window,
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("display-name").is_none());
            assert!(window.try_find("ok").is_some());
            assert!(
                window
                    .find("startup-error")
                    .label()
                    .unwrap()
                    .contains("Fix settings.json")
            );
        })
        .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
