use gpui_kit::{
    component::{
        ActiveTheme, WindowExt,
        button::{Button, ButtonVariants},
        dialog::{DialogAction, DialogFooter},
        input::{Input, InputState},
    },
    *,
};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSArray, NSURL};
use std::{
    ffi::CString,
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn get_info_command(path: &Path) -> Command {
    let mut command = Command::new("/usr/bin/osascript");
    command.args(["-e", "on run argv\nset targetFile to POSIX file (item 1 of argv) as alias\ntell application \"Finder\"\nopen information window of targetFile\nactivate\nend tell\nend run"]);
    command.arg(path);
    command
}

pub(crate) fn copy_file(path: &Path, pasteboard: &NSPasteboard) -> Result<(), String> {
    std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    let url = NSURL::from_file_path(path).ok_or("Invalid file path")?;
    let objects = NSArray::from_slice(&[ProtocolObject::from_ref(&*url)]);
    pasteboard.clearContents();
    if pasteboard.writeObjects(&objects) {
        Ok(())
    } else {
        Err("The clipboard couldn’t accept this file.".into())
    }
}

fn rename_file(path: &Path, name: &str) -> io::Result<()> {
    if name.trim().is_empty() || matches!(name, "." | "..") || name.contains(['/', ':', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Enter a filename without /, :, or null characters.",
        ));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "This location can’t be renamed.",
            )
        })?;
    let destination = parent.join(name);
    if destination == path {
        return Ok(());
    }
    let source = CString::new(path.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    // RENAME_EXCL prevents replacing another file, including during a race.
    if unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn show_rename(path: PathBuf, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        crate::settings_view::show_error(
            "This filename can’t be edited as text.".into(),
            window,
            cx,
        );
        return;
    };
    let input = cx.new(|cx| {
        let mut input = InputState::new(window, cx);
        input.set_value(name, window, cx);
        input.select_all(window, cx);
        input
    });
    let form = cx.new(|_| RenameForm {
        path,
        input: input.clone(),
        error: None,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let confirm = form.clone();
        dialog
            .title("Rename")
            .width(px(460.))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        DialogAction::new().child(Button::new("rename").primary().label("Rename")),
                    ),
            )
            .on_ok(move |_, _, cx| {
                confirm.update(cx, |form, cx| {
                    match rename_file(&form.path, &form.input.read(cx).value()) {
                        Ok(()) => true,
                        Err(error) => {
                            form.error = Some(format!("Couldn’t rename: {error}"));
                            cx.notify();
                            false
                        }
                    }
                })
            })
    });
    window.defer(cx, move |window, cx| {
        input.update(cx, |input, cx| input.focus(window, cx))
    });
}

struct RenameForm {
    path: PathBuf,
    input: Entity<InputState>,
    error: Option<String>,
}

impl Render for RenameForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                Input::new(&self.input)
                    .id("rename-input")
                    .aria_label("New filename"),
            )
            .children(self.error.clone().map(|error| {
                div()
                    .id("rename-error")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(error.clone())
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{copy_file, get_info_command, rename_file};
    use objc2_app_kit::{NSPasteboard, NSURLNSPasteboardSupport};
    use objc2_foundation::NSURL;
    use std::{
        fs, io,
        os::unix::fs::symlink,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn native_copy_and_rename_preserve_files_and_reject_invalid_or_existing_names() {
        let directory = std::env::temp_dir().join(format!(
            "everything-actions-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let file = directory.join("a 'quoted' 雪; file.txt");
        let existing = directory.join("existing.txt");
        fs::write(&file, b"source").unwrap();
        fs::write(&existing, b"destination").unwrap();
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        copy_file(&file, &pasteboard).unwrap();
        assert_eq!(
            NSURL::URLFromPasteboard(&pasteboard)
                .unwrap()
                .to_file_path(),
            Some(file.clone())
        );
        assert!(copy_file(&directory.join("missing"), &pasteboard).is_err());
        pasteboard.clearContents();
        assert_eq!(
            rename_file(&file, "existing.txt").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&existing).unwrap(), b"destination");
        for name in ["", " ", ".", "..", "../outside.txt", "a/b", "a:b", "a\0b"] {
            assert!(rename_file(&file, name).is_err(), "{name:?}");
        }
        rename_file(&file, "Renamed.txt").unwrap();
        let renamed = directory.join("Renamed.txt");
        assert_eq!(fs::read(&renamed).unwrap(), b"source");
        rename_file(&renamed, "renamed.txt").unwrap();
        let link = directory.join("link");
        symlink(directory.join("renamed.txt"), &link).unwrap();
        rename_file(&link, "renamed-link").unwrap();
        assert!(
            fs::symlink_metadata(directory.join("renamed-link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(directory.join("renamed.txt")).unwrap(), b"source");
        let folder = directory.join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("nested.txt"), b"nested").unwrap();
        rename_file(&folder, "renamed-folder").unwrap();
        assert_eq!(
            fs::read(directory.join("renamed-folder/nested.txt")).unwrap(),
            b"nested"
        );
        assert!(rename_file(&directory.join("missing"), "new.txt").is_err());
        let command = get_info_command(&renamed);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(command.get_program(), "/usr/bin/osascript");
        assert_eq!(args[2], renamed.as_os_str());
        let compiled = Command::new("/usr/bin/osacompile")
            .arg("-o")
            .arg(directory.join("info.scpt"))
            .args([args[0], args[1]])
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
