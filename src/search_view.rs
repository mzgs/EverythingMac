use crate::{
    index::{Entry, Index, Snapshot},
    query::Query,
    settings::Settings,
};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Icon, Selectable, Sizable,
        button::{Button, ButtonVariants},
        input::{Input, InputEvent, InputState},
        scroll::Scrollbar,
    },
    *,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

actions!(
    everything,
    [
        FocusSearch,
        RebuildIndex,
        OpenResult,
        RevealResult,
        CopyPath,
        NextResult,
        PreviousResult,
        SearchHelp
    ]
);

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Name,
    Path,
    Size,
    Modified,
}

pub(crate) struct SearchView {
    input: Entity<InputState>,
    index: Index,
    roots: Vec<PathBuf>,
    database_path: Option<PathBuf>,
    snapshot: Snapshot,
    results: Arc<Vec<Arc<Entry>>>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    case: bool,
    path: bool,
    regex: bool,
    filter: usize,
    sort: Sort,
    descending: bool,
    searching: bool,
    pending_open: Option<bool>,
    error: Option<String>,
    generation: Arc<AtomicU64>,
    _subscription: Subscription,
    _poll: Task<()>,
    search_task: Option<Task<()>>,
}

impl SearchView {
    pub(crate) fn new(
        database_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let roots = cx.global::<Settings>().roots.clone();
        let index = Index::start(roots.clone(), database_path.clone());
        let snapshot = index.snapshot();
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search files and folders…"));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::Change => this.search(window, cx, true),
                InputEvent::PressEnter { secondary, .. } => this.open(*secondary, window, cx),
                _ => {}
            });
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| {
                        let roots = cx.global::<Settings>().roots.clone();
                        if roots != this.roots {
                            this.roots = roots.clone();
                            this.index.stop();
                            this.index = Index::start(roots, this.database_path.clone());
                            this.snapshot = this.index.snapshot();
                            this.search(window, cx, true);
                        }
                        let snapshot = this.index.snapshot();
                        if snapshot.revision != this.snapshot.revision {
                            this.snapshot = snapshot;
                            this.search(window, cx, false);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut view = Self {
            input,
            index,
            roots,
            database_path,
            snapshot,
            results: Arc::new(Vec::new()),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            case: false,
            path: false,
            regex: false,
            filter: 0,
            sort: Sort::Name,
            descending: false,
            searching: false,
            pending_open: None,
            error: None,
            generation: Arc::new(AtomicU64::new(0)),
            _subscription: subscription,
            _poll: poll,
            search_task: None,
        };
        view.search(window, cx, true);
        view
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>, reveal_selection: bool) {
        self.pending_open = None;
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let query = Query::parse(
            &self.input.read(cx).value(),
            self.case,
            self.path,
            self.regex,
        );
        let query = match query {
            Ok(query) => query,
            Err(error) => {
                self.error = Some(error);
                self.results = Arc::new(Vec::new());
                self.selected = None;
                self.searching = false;
                cx.notify();
                return;
            }
        };
        self.error = None;
        self.searching = true;
        let entries = self.snapshot.entries.clone();
        let filter = self.filter;
        let sort = self.sort;
        let descending = self.descending;
        let current = self.generation.clone();
        let selected_path = self
            .selected
            .and_then(|i| self.results.get(i))
            .map(|e| e.path.clone());
        self.search_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let result = cx
                .background_spawn(async move {
                    if current.load(Ordering::Relaxed) != generation {
                        return None;
                    }
                    let mut results = Vec::new();
                    for (i, entry) in entries.iter().enumerate() {
                        if i % 1024 == 0 && current.load(Ordering::Relaxed) != generation {
                            return None;
                        }
                        if kind_matches(entry, filter)
                            && query.matches(&entry.path, entry.is_dir, entry.size)
                        {
                            results.push(entry.clone());
                        }
                    }
                    match sort {
                        Sort::Name => results.sort_by_cached_key(|e| {
                            (
                                e.path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_lowercase(),
                                e.path.clone(),
                            )
                        }),
                        Sort::Path => results.sort_by(|a, b| a.path.cmp(&b.path)),
                        Sort::Size => results
                            .sort_by(|a, b| a.size.cmp(&b.size).then_with(|| a.path.cmp(&b.path))),
                        Sort::Modified => results.sort_by(|a, b| {
                            a.modified
                                .cmp(&b.modified)
                                .then_with(|| a.path.cmp(&b.path))
                        }),
                    }
                    if descending {
                        results.reverse();
                    }
                    Some(Arc::new(results))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation.load(Ordering::Relaxed) != generation {
                    return;
                }
                if let Some(results) = result {
                    this.selected = selected_path
                        .and_then(|path| results.iter().position(|entry| entry.path == path))
                        .or_else(|| (!results.is_empty()).then_some(0));
                    this.results = results;
                    this.searching = false;
                    if reveal_selection {
                        this.scroll
                            .scroll_to_item(this.selected.unwrap_or(0), ScrollStrategy::Top);
                    }
                    if let Some(reveal) = this.pending_open.take() {
                        this.open(reveal, window, cx);
                    }
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn open(&mut self, reveal: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.searching {
            self.pending_open = Some(reveal);
            return;
        }
        let Some(entry) = self.selected.and_then(|i| self.results.get(i)) else {
            return;
        };
        let path = entry.path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let mut command = std::process::Command::new("/usr/bin/open");
                    if reveal {
                        command.arg("-R");
                    }
                    command.arg(&path);
                    let output = command.output()?;
                    if output.status.success() {
                        Ok(())
                    } else {
                        Err(std::io::Error::other(
                            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                        ))
                    }
                })
                .await;
            if let Err(error) = result {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("Couldn’t open result: {error}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn copy(&self, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected.and_then(|i| self.results.get(i)) {
            cx.write_to_clipboard(ClipboardItem::new_string(
                entry.path.to_string_lossy().into_owned(),
            ));
        }
    }

    fn move_selection(&mut self, direction: isize, cx: &mut Context<Self>) {
        if self.results.is_empty() {
            return;
        }
        self.selected = Some(
            self.selected
                .unwrap_or(0)
                .saturating_add_signed(direction)
                .min(self.results.len() - 1),
        );
        self.scroll
            .scroll_to_item(self.selected.unwrap(), ScrollStrategy::Top);
        cx.notify();
    }

    fn heading(
        &self,
        label: &'static str,
        sort: Sort,
        width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(label)
            .ghost()
            .small()
            .accessibility_label(label)
            .tooltip(format!("Sort by {}", label.to_lowercase()))
            .w(px(width))
            .px_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .w_full()
                    .child(label)
                    .when(self.sort == sort, |this| {
                        this.child(
                            Icon::new(if self.descending {
                                IconName::ArrowDown
                            } else {
                                IconName::ArrowUp
                            })
                            .xsmall(),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.sort == sort {
                    this.descending = !this.descending;
                } else {
                    this.sort = sort;
                    this.descending = false;
                }
                this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                this.search(window, cx, false);
            }))
    }
}

impl Render for SearchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let results = self.results.clone();
        let selected = self.selected;
        let owner = cx.entity().downgrade();
        let no_results = self.results.is_empty();
        let (empty_icon, empty_title, empty_description) = if self.roots.is_empty() {
            (
                IconName::FolderPlus,
                "Find your files, instantly",
                "Add a folder or volume to start. Subfolders are included.",
            )
        } else if self.error.is_some() {
            (
                IconName::CircleAlert,
                "Let’s fix that search",
                "Check the message above, or start with a fresh search.",
            )
        } else if self.snapshot.scanning {
            (
                IconName::HardDrive,
                "Getting your files ready",
                "Results appear as folders are indexed. You can keep searching.",
            )
        } else if self.searching {
            (
                IconName::Search,
                "Searching your index…",
                "Finding files and folders that match your search.",
            )
        } else {
            (
                IconName::SearchX,
                "No matches found",
                "Try fewer words, a different file type, or reset your search.",
            )
        };
        div().size_full().flex().flex_col().bg(cx.theme().title_bar).text_color(cx.theme().foreground)
            .key_context("Everything")
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| this.input.update(cx, |input, cx| input.focus(window, cx))))
            .on_action(cx.listener(|this, _: &RebuildIndex, _, cx| { this.index.refresh(); cx.notify(); }))
            .on_action(cx.listener(|this, _: &OpenResult, window, cx| this.open(false, window, cx)))
            .on_action(cx.listener(|this, _: &RevealResult, window, cx| this.open(true, window, cx)))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &NextResult, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousResult, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|_, _: &SearchHelp, window, cx| crate::settings_view::show_error(
                "Search filenames as you type.\n\nreport budget — both words\n\"annual report\" — exact phrase\n*.pdf | *.docx — either pattern\n!backup — exclude a word\n<cat | dog> photo — group terms\next:jpg;png — extension filter\nfile: / folder: — kind\npath:Projects — search full path\nsize:>10mb — size filter\n\nAa: case sensitive. Path: match full paths. Regex: treat the entire query as a regular expression.\n\n⌘F Search · ↓/↑ Select · Return Open · ⌘Return Reveal · ⇧⌘C Copy path · ⌘R Rebuild · ⌘, Folders".into(), window, cx)))
            .child(div().flex().items_center().justify_between().px_5().pt_4().pb_3()
                .child(div().flex().items_center().gap_3()
                    .child(div().flex().items_center().justify_center().size_9().rounded_lg().bg(cx.theme().link.opacity(0.12))
                        .child(Icon::new(IconName::Search).size_5().text_color(cx.theme().link)))
                    .child(div().flex().flex_col().gap_1()
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Everything Mac"))
                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Your files, a few keystrokes away."))))
                .child(div().flex().items_center().gap_2()
                    .child(Button::new("folders").icon(IconName::FolderCog).label("Indexed folders").tooltip("Choose folders to search · ⌘,").on_click(|_, window, cx| window.dispatch_action(Box::new(crate::OpenSettings), cx)))
                    .child(Button::new("rebuild").ghost().icon(IconName::RefreshCw).label("Rebuild").tooltip("Rescan indexed folders · ⌘R").disabled(self.snapshot.scanning).on_click(cx.listener(|this, _, _, cx| { this.index.refresh(); cx.notify(); })))
                    .child(Button::new("help").ghost().icon(IconName::CircleQuestionMark).accessibility_label("Search help").tooltip("Search syntax and keyboard shortcuts").on_click(|_, window, cx| window.dispatch_action(Box::new(SearchHelp), cx)))
                    .child(Button::new("full-disk-access").ghost().icon(IconName::TriangleAlert).accessibility_label("Needs Full Disk Access")
                        .tooltip(self.snapshot.scan_warning.clone().unwrap_or_else(|| "Open Full Disk Access in System Settings".into()))
                        .on_click(|_, _, cx| cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")))))
            .child(div().px_5().pb_3().child(Input::new(&self.input).id("search-input").aria_label("Search files and folders")
                .prefix(Icon::new(IconName::Search).small().text_color(cx.theme().muted_foreground))
                .suffix(div().text_xs().text_color(cx.theme().muted_foreground).child("⌘ F"))
                .cleanable(true)))
            .child(div().flex().flex_wrap().items_center().justify_between().px_5().pb_3().gap_2()
                .child(div().flex().items_center().gap_1().children([
                    ("Everything", IconName::Layers), ("Files", IconName::File), ("Folders", IconName::Folder),
                    ("Audio", IconName::Music2), ("Images", IconName::Image), ("Videos", IconName::Video), ("Documents", IconName::FileText)
                ].into_iter().enumerate().map(|(filter, (label, icon))| {
                    Button::new(("filter", filter)).ghost().small().icon(icon).label(label).selected(self.filter == filter)
                        .when(self.filter == filter, |this| this.bg(cx.theme().link.opacity(0.12)).text_color(cx.theme().link))
                        .on_click(cx.listener(move |this, _, window, cx| { this.filter = filter; this.search(window, cx, true); }))
                })))
                .child(div().flex().items_center().gap_1()
                    .child(Button::new("case").ghost().small().label("Aa").tooltip("Case sensitive").selected(self.case).on_click(cx.listener(|this, _, window, cx| { this.case = !this.case; this.search(window, cx, true); })))
                    .child(Button::new("path").ghost().small().label("Path").tooltip("Search full paths").selected(self.path).on_click(cx.listener(|this, _, window, cx| { this.path = !this.path; this.search(window, cx, true); })))
                    .child(Button::new("regex").ghost().small().label("Regex").tooltip("Use a regular expression").selected(self.regex).on_click(cx.listener(|this, _, window, cx| { this.regex = !this.regex; this.search(window, cx, true); })))))
            .children(self.error.clone().map(|error| div().id("search-error").test_support().role(Role::Status).aria_label(error.clone())
                .mx_5().mb_3().px_3().py_2().rounded_md().bg(cx.theme().danger.opacity(0.08)).flex().items_center().gap_2().text_sm().text_color(cx.theme().danger)
                .child(Icon::new(IconName::CircleAlert).small().flex_shrink_0()).child(div().flex_1().min_w_0().child(error))))
            .child(div().flex().items_center().justify_between().px_5().py_2().border_t_1().border_color(cx.theme().border).bg(cx.theme().table)
                .child(div().flex().items_center().gap_2()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Results"))
                    .child(div().id("result-count").test_support().role(Role::Status).aria_label(format!("{} matches", self.results.len()))
                        .text_xs().text_color(cx.theme().muted_foreground).child(format!("{} matches", format_count(self.results.len()))))))
            .child(div().flex().items_center().px_5().border_y_1().border_color(cx.theme().border).bg(cx.theme().muted).text_color(cx.theme().muted_foreground)
                .child(self.heading("Name", Sort::Name, 280., cx))
                .child(div().flex_1().min_w_0().child(self.heading("Path", Sort::Path, 140., cx)))
                .child(self.heading("Size", Sort::Size, 90., cx))
                .child(div().w(px(90.)).text_sm().py_2().child("Type"))
                .child(self.heading("Modified", Sort::Modified, 110., cx)))
            .child(div().id("results-table").test_support().role(Role::Table).aria_label(format!("{} results", self.results.len())).aria_row_count(self.results.len()).relative().flex_1().min_h_0().bg(cx.theme().table).child(
                uniform_list("results", results.len(), move |range, _, cx| {
                    range.map(|i| {
                        let entry = &results[i];
                        let name = entry.path.file_name().unwrap_or(entry.path.as_os_str()).to_string_lossy().into_owned();
                        let parent = entry.path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                        let kind = if entry.is_dir { "Folder".into() } else { entry.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_else(|| "File".into()) };
                        let owner = owner.clone();
                        div().id(("result", i)).test_support().role(Role::Row).aria_label(entry.path.to_string_lossy().into_owned())
                            .aria_selected(selected == Some(i)).flex().items_center().h(px(36.)).px_5().text_sm().w_full().cursor_pointer()
                            .when(i % 2 == 1, |row| row.bg(cx.theme().table_even))
                            .hover(|row| row.bg(if selected == Some(i) { cx.theme().link.opacity(0.18) } else { cx.theme().table_hover }))
                            .when(selected == Some(i), |row| row.bg(cx.theme().link.opacity(0.12)))
                            .child(div().w(px(280.)).flex_shrink_0().pr_3().flex().items_center().gap_2()
                                .child(Icon::new(if entry.is_dir { IconName::Folder } else { IconName::File }).small().flex_shrink_0()
                                    .text_color(if entry.is_dir { cx.theme().link } else { cx.theme().muted_foreground }))
                                .child(div().min_w_0().truncate().child(name)))
                            .child(div().flex_1().min_w_0().pr_3().truncate().text_color(cx.theme().muted_foreground).child(parent))
                            .child(div().w(px(90.)).flex_shrink_0().child(if entry.is_dir { "—".into() } else { format_size(entry.size) }))
                            .child(div().w(px(90.)).flex_shrink_0().truncate().child(kind))
                            .child(div().w(px(110.)).flex_shrink_0().child(format_modified(entry.modified)))
                            .on_click(move |event, window, cx| {
                                let _ = owner.update(cx, |this, cx| {
                                    this.selected = Some(i);
                                    if event.click_count() == 2 { this.open(false, window, cx); }
                                    cx.notify();
                                });
                            }).into_any_element()
                    }).collect()
                }).size_full().track_scroll(&self.scroll)
            ).child(Scrollbar::vertical(&self.scroll))
                .when(no_results, |this| this.child(div().absolute().inset_0().flex().flex_col().gap_3().items_center().justify_center().px_5()
                    .child(div().flex().items_center().justify_center().size_12().rounded_lg().bg(cx.theme().muted)
                        .child(Icon::new(empty_icon).size_6().text_color(cx.theme().muted_foreground)))
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(empty_title))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(empty_description))
                    .when(self.roots.is_empty(), |this| this.child(Button::new("empty-add-folders").primary().icon(IconName::FolderPlus).label("Add indexed folders")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::OpenSettings), cx))))
                    .when(!self.roots.is_empty() && !self.snapshot.scanning && !self.searching, |this| this.child(Button::new("reset-search").label("Reset search")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.filter = 0;
                            this.case = false;
                            this.path = false;
                            this.regex = false;
                            this.input.update(cx, |input, cx| { input.set_value("", window, cx); input.focus(window, cx); });
                            this.search(window, cx, true);
                        })))))))
            .child(div().flex().items_center().gap_2().px_5().py_3().border_t_1().border_color(cx.theme().border).bg(cx.theme().status_bar)
                .child(Button::new("open").primary().small().icon(IconName::ExternalLink).label("Open").tooltip("Open selected result · Return").disabled(self.selected.is_none()).on_click(cx.listener(|this, _, window, cx| this.open(false, window, cx))))
                .child(Button::new("reveal").ghost().small().icon(IconName::FolderOpen).label("Show in Finder").tooltip("Reveal selected result · ⌘Return").disabled(self.selected.is_none()).on_click(cx.listener(|this, _, window, cx| this.open(true, window, cx))))
                .child(Button::new("copy-path").ghost().small().icon(IconName::Copy).label("Copy path").tooltip("Copy full path · ⇧⌘C").disabled(self.selected.is_none()).on_click(cx.listener(|this, _, _, cx| this.copy(cx))))
                .child(div().flex_1())
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child("↑ ↓ Navigate    ↵ Open    ⌘ ↵ Reveal")))
            .children(self.snapshot.warning.clone().map(|warning| div().id("index-warnings").test_support().role(Role::Status).aria_label(warning.clone()).max_h(px(80.)).overflow_y_scroll().px_5().pb_2().bg(cx.theme().status_bar).text_xs().text_color(cx.theme().muted_foreground).child(warning)))
    }
}

fn kind_matches(entry: &Entry, filter: usize) -> bool {
    if filter == 0 {
        return true;
    }
    if filter == 1 {
        return !entry.is_dir;
    }
    if filter == 2 {
        return entry.is_dir;
    }
    if entry.is_dir {
        return false;
    }
    let ext = entry
        .path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let extensions: &[&str] = match filter {
        3 => &["mp3", "m4a", "aac", "flac", "wav", "aiff", "ogg", "opus"],
        4 => &[
            "jpg", "jpeg", "png", "gif", "webp", "heic", "tiff", "bmp", "svg", "raw",
        ],
        5 => &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mpeg"],
        6 => &[
            "pdf", "doc", "docx", "txt", "md", "rtf", "pages", "xls", "xlsx", "csv", "ppt", "pptx",
            "key", "numbers", "odt",
        ],
        _ => &[],
    };
    extensions.contains(&ext.as_str())
}

fn format_count(count: usize) -> String {
    if count >= 1_000_000_000 {
        format!("{:.1}B", count as f64 / 1_000_000_000.)
    } else if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.)
    } else {
        count.to_string()
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in ["KB", "MB", "GB", "TB", "PB", "EB"] {
        value /= 1024.;
        unit = next;
        if value < 1024. {
            break;
        }
    }
    format!("{value:.1} {unit}")
}

fn format_modified(time: Option<SystemTime>) -> String {
    let Some(time) = time else {
        return "—".into();
    };
    let Ok(age) = SystemTime::now().duration_since(time) else {
        return "Future".into();
    };
    match age.as_secs() {
        0..60 => "Just now".into(),
        60..3600 => format!("{}m ago", age.as_secs() / 60),
        3600..86400 => format!("{}h ago", age.as_secs() / 3600),
        seconds => format!("{}d ago", seconds / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::{SearchView, format_count, format_size, kind_matches};
    use crate::{index::Entry, settings::Settings};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, ScrollStrategy, TestAppContext};
    use std::{path::PathBuf, sync::Arc};

    #[gpui_kit::test]
    fn access_icon_stays_in_header_and_opens_full_disk_access(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| cx.set_global(Settings { roots: Vec::new() }));
        let (window, view) = cx.update(|cx| {
            cx.set_reduce_motion(true);
            gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |window, cx| {
                window.set_scroll_layers(false);
                cx.new(|cx| SearchView::new(None, window, cx))
            })
            .unwrap()
        });
        for active in [true, false] {
            cx.update(|cx| {
                view.update(cx, |view, cx| {
                    view._poll = gpui_kit::Task::ready(());
                    view.search_task = None;
                    view.searching = active;
                    view.snapshot.scanning = active;
                    view.snapshot.skipped = if active { 111 } else { 0 };
                    view.snapshot.scan_warning = active.then(|| "111 inaccessible paths".into());
                    view.snapshot.warning = Some("Index database unavailable".into());
                    cx.notify();
                });
            });
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(window.try_find("index-status").is_none());
                assert_eq!(
                    window.find("full-disk-access").label(),
                    Some("Needs Full Disk Access")
                );
                assert_eq!(
                    window.find("index-warnings").label(),
                    Some("Index database unavailable")
                );
            })
            .unwrap();
        }
        cx.update_window(window.into(), |_, window, cx| {
            window.click("full-disk-access", cx);
            window.render_frame(cx);
            assert!(window.try_find("dialog").is_none());
        })
        .unwrap();
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        );
    }

    #[gpui_kit::test]
    async fn background_refresh_preserves_scroll_and_sorting_goes_to_top(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.set_global(Settings { roots: Vec::new() });
        });
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1150.), gpui_kit::px(800.)),
            |window, cx| {
                window.set_scroll_layers(false);
                SearchView::new(None, window, cx)
            },
        );
        let task = window
            .update(cx, |view, window, cx| {
                view._poll = gpui_kit::Task::ready(());
                view.snapshot.entries = Arc::new(
                    (0..500)
                        .map(|i| {
                            Arc::new(Entry {
                                path: PathBuf::from(format!("/fixture/file-{i:03}.txt")),
                                is_dir: false,
                                size: 123,
                                modified: None,
                            })
                        })
                        .collect(),
                );
                view.search(window, cx, true);
                view.search_task.take().unwrap()
            })
            .unwrap();
        task.await;
        window
            .update(cx, |view, _, cx| {
                view.scroll.scroll_to_item(200, ScrollStrategy::Top);
                cx.notify();
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("result", 200usize)).is_some());
            assert!(window.try_find(("result", 0usize)).is_none());
        })
        .unwrap();
        let offset = window
            .update(cx, |view, _, _| view.scroll.0.borrow().base_handle.offset())
            .unwrap();
        let task = window
            .update(cx, |view, window, cx| {
                view.search(window, cx, false);
                view.search_task.take().unwrap()
            })
            .unwrap();
        task.await;
        cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.selected, Some(0));
                assert_eq!(view.scroll.0.borrow().base_handle.offset(), offset);
            })
            .unwrap();
        for heading in [
            "Name", "Name", "Size", "Size", "Modified", "Modified", "Path", "Path",
        ] {
            window
                .update(cx, |view, _, cx| {
                    view.scroll.scroll_to_item(200, ScrollStrategy::Top);
                    cx.notify();
                })
                .unwrap();
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                window.click(heading, cx);
            })
            .unwrap();
            let task = window
                .update(cx, |view, _, _| view.search_task.take().unwrap())
                .unwrap();
            task.await;
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(window.try_find(("result", 0usize)).is_some(), "{heading}");
                assert!(window.try_find(("result", 200usize)).is_none(), "{heading}");
            })
            .unwrap();
            window
                .update(cx, |view, _, _| {
                    assert_eq!(
                        view.scroll.0.borrow().base_handle.offset().y,
                        gpui_kit::px(0.)
                    );
                    assert_eq!(
                        view.results[view.selected.unwrap()].path,
                        PathBuf::from("/fixture/file-000.txt")
                    );
                    assert_eq!(
                        view.results[0].path,
                        PathBuf::from(if view.descending {
                            "/fixture/file-499.txt"
                        } else {
                            "/fixture/file-000.txt"
                        })
                    );
                })
                .unwrap();
        }
        let task = window
            .update(cx, |view, window, cx| {
                view.search(window, cx, true);
                view.search_task.take().unwrap()
            })
            .unwrap();
        task.await;
        let selected = window
            .update(cx, |view, _, _| view.selected.unwrap())
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(("result", selected)).is_some());
            assert!(window.try_find(("result", 200usize)).is_none());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn renders_and_scrolls_a_million_results_without_scroll_tiles(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.set_global(Settings { roots: Vec::new() });
        });
        let window = cx.add_window(|window, cx| {
            // The native entry point sets GPUI_SCROLL_LAYERS=0 before starting GPUI.
            window.set_scroll_layers(false);
            SearchView::new(None, window, cx)
        });
        window
            .update(cx, |view, _, _| {
                let entry = Arc::new(Entry {
                    path: PathBuf::from("/fixture/file.txt"),
                    is_dir: false,
                    size: 123,
                    modified: None,
                });
                view.results = Arc::new(vec![entry; 1_000_000]);
                view.selected = Some(0);
            })
            .unwrap();
        for (index, hidden) in [(0usize, 999_999usize), (999_999, 0), (0, 999_999)] {
            window
                .update(cx, |view, _, cx| {
                    view.scroll.scroll_to_item(index, ScrollStrategy::Top);
                    cx.notify();
                })
                .unwrap();
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(window.try_find(("result", index)).is_some());
                assert!(
                    window.try_find(("result", hidden)).is_none(),
                    "Rows stay virtualized"
                );
            })
            .unwrap();
        }
    }

    #[test]
    fn formats_result_counts() {
        for (count, expected) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1.0K"),
            (12_500, "12.5K"),
            (999_000, "999.0K"),
            (1_000_000, "1.0M"),
            (1_250_000, "1.2M"),
            (1_000_000_000, "1.0B"),
            (1_500_000_000, "1.5B"),
        ] {
            assert_eq!(format_count(count), expected);
        }
    }

    #[test]
    fn formats_sizes_and_filters_extensions_without_case_sensitivity() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        let mut entry = Entry {
            path: "/tmp/photo.JPG".into(),
            is_dir: false,
            size: 1,
            modified: None,
        };
        assert!(kind_matches(&entry, 4));
        assert!(!kind_matches(&entry, 3));
        entry.is_dir = true;
        assert!(kind_matches(&entry, 2));
        assert!(!kind_matches(&entry, 4));
    }
}
