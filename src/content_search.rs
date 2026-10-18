use std::{
    collections::HashSet,
    ffi::OsString,
    io::{BufRead, BufReader, ErrorKind},
    os::{fd::AsRawFd, unix::ffi::OsStringExt},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub(crate) fn predicate(text: &str, match_case: bool) -> Result<Option<String>, String> {
    if text.len() > 4096 {
        return Err("Search is too long (maximum 4096 bytes).".into());
    }
    if text.chars().any(char::is_control) {
        return Err("Content search cannot contain control characters.".into());
    }
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    // mdfind needs suffix modifiers; NSPredicate's ==[c] syntax produces different matches.
    let modifier = if match_case { "" } else { "c" };
    Ok(Some(format!(
        "kMDItemTextContent == \"*{escaped}*\"{modifier}"
    )))
}

pub(crate) fn search(
    predicate: Option<&str>,
    roots: &[PathBuf],
    current: &AtomicU64,
    generation: u64,
) -> Result<Option<HashSet<PathBuf>>, String> {
    let mut paths = HashSet::new();
    let Some(predicate) = predicate else {
        return Ok(Some(paths));
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    for (i, root) in roots.iter().enumerate() {
        if crate::index::is_cloud_path(root)
            || roots
                .iter()
                .enumerate()
                .any(|(j, parent)| j != i && root.starts_with(parent) && (root != parent || j < i))
        {
            continue;
        }
        if current.load(Ordering::Relaxed) != generation {
            return Ok(None);
        }
        let mut command = Command::new("/usr/bin/mdfind");
        command
            .args(["-0", "-literal", "-onlyin"])
            .arg(root)
            .arg(predicate);
        if !collect(&mut command, &mut paths, deadline, || {
            current.load(Ordering::Relaxed) != generation
        })? {
            return Ok(None);
        }
    }
    Ok(Some(paths))
}

fn collect(
    command: &mut Command,
    paths: &mut HashSet<PathBuf>,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Could not start Spotlight search: {error}"))?;
    let result = (|| {
        let stdout = child.stdout.take().unwrap();
        let fd = stdout.as_raw_fd();
        // Nonblocking reads let a superseded query stop even while Spotlight is busy.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err("Could not read Spotlight search results.".into());
            }
        }
        let mut stdout = BufReader::new(stdout);
        let mut bytes = Vec::new();
        loop {
            if cancelled() {
                return Ok(false);
            }
            if Instant::now() >= deadline {
                return Err("Spotlight search timed out. Try a more specific search.".into());
            }
            match stdout.read_until(0, &mut bytes) {
                Ok(0) => {
                    if !bytes.is_empty() {
                        return Err("Spotlight returned an incomplete result path.".into());
                    }
                    if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                        return if status.success() {
                            Ok(true)
                        } else {
                            Err(format!("Spotlight search failed ({status})."))
                        };
                    }
                }
                Ok(_) => {
                    if bytes.pop() != Some(0) {
                        return Err("Spotlight returned an incomplete result path.".into());
                    }
                    let path = PathBuf::from(OsString::from_vec(std::mem::take(&mut bytes)));
                    if !path.is_absolute() {
                        return Err("Spotlight returned an invalid result path.".into());
                    }
                    paths.insert(path);
                    continue;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(format!("Could not read Spotlight results: {error}")),
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_predicate_escapes_input_and_keeps_wildcards() {
        assert_eq!(predicate("  ", false).unwrap(), None);
        assert_eq!(
            predicate("annual report", false).unwrap().unwrap(),
            "kMDItemTextContent == \"*annual report*\"c"
        );
        assert_eq!(
            predicate(r#"a\b" || kMDItemFSName == "*"#, true)
                .unwrap()
                .unwrap(),
            r#"kMDItemTextContent == "*a\\b\" || kMDItemFSName == \"**""#
        );
        assert!(predicate("bad\0query", false).is_err());
        assert!(predicate(&"a".repeat(4097), false).is_err());
    }

    #[test]
    fn spotlight_output_preserves_paths_and_reports_failures() {
        let mut paths = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut command = Command::new("/usr/bin/printf");
        command.arg("/a file\\000/line\\nbreak\\000/raw\\377\\000/a file\\000");
        assert!(collect(&mut command, &mut paths, deadline, || false).unwrap());
        assert_eq!(paths.len(), 3);
        assert!(paths.contains(&PathBuf::from("/line\nbreak")));
        assert!(paths.contains(&PathBuf::from(OsString::from_vec(b"/raw\xff".to_vec()))));
        assert!(
            collect(
                &mut Command::new("/usr/bin/false"),
                &mut paths,
                deadline,
                || false
            )
            .is_err()
        );
        let mut command = Command::new("/usr/bin/printf");
        command.arg("/incomplete");
        assert!(collect(&mut command, &mut paths, deadline, || false).is_err());
        let mut command = Command::new("/usr/bin/printf");
        command.arg("relative\\000");
        assert!(collect(&mut command, &mut paths, deadline, || false).is_err());
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf '/split'; sleep 0.03; printf ' path\\000'"]);
        assert!(collect(&mut command, &mut paths, deadline, || false).unwrap());
        assert!(paths.contains(&PathBuf::from("/split path")));
    }

    #[test]
    fn cancelled_and_timed_out_searches_stop_the_child() {
        for cancelled in [true, false] {
            let mut command = Command::new("/bin/sleep");
            command.arg("5");
            let started = Instant::now();
            let checks = std::cell::Cell::new(0);
            let result = collect(
                &mut command,
                &mut HashSet::new(),
                started + Duration::from_millis(50),
                || {
                    checks.set(checks.get() + 1);
                    cancelled && checks.get() >= 3
                },
            );
            if cancelled {
                assert!(!result.unwrap());
            } else {
                assert!(result.unwrap_err().contains("timed out"));
            }
            assert!(started.elapsed() < Duration::from_secs(1));
        }
        let current = AtomicU64::new(2);
        assert!(
            search(Some("unused"), &[PathBuf::from("/")], &current, 1)
                .unwrap()
                .is_none()
        );
        assert!(
            search(None, &[PathBuf::from("/")], &current, 2)
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }
}
