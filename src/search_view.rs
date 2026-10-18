use crate::{
    index::{Entry, Index, Snapshot},
    query::Query,
    settings::Settings,
};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Icon, Selectable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        input::{Input, InputEvent, InputState},
        menu::{ContextMenuExt, PopupMenuItem},
        progress::Progress,
        scroll::Scrollbar,
        spinner::Spinner,
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
        QuickLook,
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

struct FilterResults {
    results: Arc<Vec<Arc<Entry>>>,
    selected: Option<usize>,
    sort: Sort,
    descending: bool,
}

pub(crate) struct SearchView {
    input: Entity<InputState>,
    results_focus: FocusHandle,
    preview: Option<crate::quick_look::Preview>,
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
    filter_results: [Option<FilterResults>; 7],
    sort: Sort,
    descending: bool,
    searching: bool,
    sorting: bool,
    starting: bool,
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
        let starting = !roots.is_empty();
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
                        this.update_snapshot(snapshot, window, cx);
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
            results_focus: cx.focus_handle().tab_stop(true),
            preview: None,
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
            filter_results: std::array::from_fn(|_| None),
            sort: Sort::Path,
            descending: false,
            searching: false,
            sorting: false,
            starting,
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

    pub(crate) fn is_scanning(&self) -> bool {
        self.snapshot.scanning
    }

    pub(crate) fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.index.refresh();
        cx.notify();
    }

    fn update_snapshot(&mut self, snapshot: Snapshot, window: &mut Window, cx: &mut Context<Self>) {
        if snapshot.revision != self.snapshot.revision {
            self.filter_results.fill_with(|| None);
            self.snapshot = snapshot;
            if self.search_task.is_none() {
                self.search(window, cx, false);
            }
        }
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>, reveal_selection: bool) {
        if reveal_selection {
            self.filter_results.fill_with(|| None);
        }
        self.pending_open = None;
        self.search_task = None;
        self.sorting = false;
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        if self.starting && !self.snapshot.loaded {
            self.searching = false;
            cx.notify();
            return;
        }
        let query = Query::parse(
            &self.input.read(cx).value(),
            self.case,
            self.path,
            self.regex,
        );
        let query = match query {
            Ok(query) => query,
            Err(error) => {
                self.starting = false;
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
        let revision = self.snapshot.revision;
        let loaded = self.snapshot.loaded;
        let entries = self.snapshot.entries.clone();
        let filter = self.filter;
        let sort = self.sort;
        let descending = self.descending;
        let current = self.generation.clone();
        self.search_task = Some(cx.spawn_in(window, async move |this, cx| {
            if reveal_selection {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
            }
            let matching_generation = current.clone();
            let result = cx
                .background_spawn(async move {
                    if matching_generation.load(Ordering::Relaxed) != generation {
                        return None;
                    }
                    let mut results = Vec::new();
                    for (i, entry) in entries.iter().enumerate() {
                        if i % 1024 == 0
                            && matching_generation.load(Ordering::Relaxed) != generation
                        {
                            return None;
                        }
                        if kind_matches(entry, filter)
                            && query.matches(&entry.path, entry.is_dir, entry.size)
                        {
                            results.push(entry.clone());
                        }
                    }
                    Some(Arc::new(results))
                })
                .await;
            let Some(mut results) = result else { return };
            if (sort != Sort::Path || descending) && results.len() > 1 {
                let published = this.update_in(cx, |this, _, cx| {
                    if this.generation.load(Ordering::Relaxed) != generation {
                        return false;
                    }
                    this.searching = false;
                    this.sorting = true;
                    if loaded {
                        this.starting = false;
                    }
                    this.set_results(results.clone(), cx, reveal_selection);
                    true
                });
                if !matches!(published, Ok(true)) {
                    return;
                }
                let sorted = cx
                    .background_spawn(async move {
                        if current.load(Ordering::Relaxed) != generation {
                            return None;
                        }
                        let mut results = Arc::unwrap_or_clone(results);
                        // shortcut: an active standard sort finishes on its worker; chunk it if rapid requests cause contention.
                        match sort {
                            // Stable sorting keeps equal names in the snapshot's path order.
                            Sort::Name => results.sort_by_cached_key(|e| {
                                e.path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_lowercase()
                            }),
                            // Snapshots arrive in path order; filtering preserves it.
                            Sort::Path => {}
                            Sort::Size => results.sort_by(|a, b| {
                                a.allocated_size
                                    .cmp(&b.allocated_size)
                                    .then_with(|| a.path.cmp(&b.path))
                            }),
                            Sort::Modified => results.sort_by(|a, b| {
                                a.modified
                                    .cmp(&b.modified)
                                    .then_with(|| a.path.cmp(&b.path))
                            }),
                        }
                        if descending {
                            results.reverse();
                        }
                        if current.load(Ordering::Relaxed) != generation {
                            return None;
                        }
                        Some(Arc::new(results))
                    })
                    .await;
                let Some(sorted) = sorted else { return };
                results = sorted;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation.load(Ordering::Relaxed) != generation {
                    return;
                }
                this.search_task = None;
                this.searching = false;
                this.sorting = false;
                this.set_results(results, cx, reveal_selection);
                if this.starting && loaded {
                    this.starting = false;
                }
                if let Some(reveal) = this.pending_open.take() {
                    this.open(reveal, window, cx);
                }
                if this.snapshot.revision == revision {
                    this.filter_results[filter] = Some(FilterResults {
                        results: this.results.clone(),
                        selected: this.selected,
                        sort,
                        descending,
                    });
                } else {
                    this.search(window, cx, false);
                }
            });
        }));
        cx.notify();
    }

    fn change_filter(&mut self, filter: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter == filter {
            return;
        }
        if !self.searching
            && !self.sorting
            && let Some(saved) = &mut self.filter_results[self.filter]
        {
            saved.selected = self.selected;
        }
        self.filter = filter;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        if let Some(saved) = &self.filter_results[filter] {
            self.generation.fetch_add(1, Ordering::Relaxed);
            self.search_task = None;
            self.pending_open = None;
            self.searching = false;
            self.sorting = false;
            self.error = None;
            self.results = saved.results.clone();
            self.selected = saved.selected;
            self.sort = saved.sort;
            self.descending = saved.descending;
            cx.notify();
        } else {
            self.sort = Sort::Path;
            self.descending = false;
            self.search(window, cx, false);
        }
    }

    fn set_results(
        &mut self,
        results: Arc<Vec<Arc<Entry>>>,
        cx: &mut Context<Self>,
        reveal_selection: bool,
    ) {
        self.selected = self
            .selected
            .and_then(|i| self.results.get(i))
            .and_then(|entry| results.iter().position(|result| result.path == entry.path))
            .or_else(|| (!results.is_empty()).then_some(0));
        self.results = results;
        if reveal_selection {
            self.scroll
                .scroll_to_item(self.selected.unwrap_or(0), ScrollStrategy::Top);
        }
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
        let mut command = std::process::Command::new("/usr/bin/open");
        if reveal {
            command.arg("-R");
        }
        command.arg(&entry.path);
        self.run_command(command, "open result", window, cx);
    }

    fn run_command(
        &mut self,
        mut command: std::process::Command,
        operation: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
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
                    this.error = Some(format!("Couldn’t {operation}: {error}"));
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

    fn trash(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { move_to_trash(&path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.error = Some(format!("Couldn’t move to Trash: {error}"));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_result(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(index);
        self.results_focus.focus(window, cx);
        cx.notify();
    }

    fn quick_look(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected.and_then(|i| self.results.get(i)) {
            let result = (|| {
                let preview = match self.preview.as_ref() {
                    Some(preview) => preview,
                    None => self.preview.insert(crate::quick_look::Preview::new()?),
                };
                preview.toggle(&entry.path)
            })();
            if let Err(error) = result {
                self.error = Some(format!("Couldn’t preview result: {error}"));
                cx.notify();
            }
        }
    }

    fn move_selection(&mut self, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.results.is_empty() {
            return;
        }
        self.select_result(
            self.selected
                .unwrap_or(0)
                .saturating_add_signed(direction)
                .min(self.results.len() - 1),
            window,
            cx,
        );
        self.scroll
            .scroll_to_item(self.selected.unwrap(), ScrollStrategy::Top);
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
            .tooltip(if sort == Sort::Size {
                "Sort by size on disk".to_string()
            } else {
                format!("Sort by {}", label.to_lowercase())
            })
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
                        this.child(if self.sorting {
                            Spinner::new().xsmall().into_any_element()
                        } else {
                            Icon::new(if self.descending {
                                IconName::ArrowDown
                            } else {
                                IconName::ArrowUp
                            })
                            .xsmall()
                            .into_any_element()
                        })
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
        let startup_loading = self.starting.then(|| {
            let stage = if !self.snapshot.loaded {
                "Loading files and folders…"
            } else {
                "Preparing your search…"
            };
            div()
                .id("startup-loading")
                .test_support()
                .role(Role::Status)
                .aria_label(stage)
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_6()
                .px_6()
                .bg(cx.theme().table)
                .text_color(cx.theme().foreground)
                .child(
                    div()
                        .size_16()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_2xl()
                        .bg(cx.theme().selection)
                        .child(
                            Icon::new(IconName::HardDrive)
                                .size_8()
                                .text_color(cx.theme().link),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .text_center()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Getting your files ready"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Your complete file list will appear when it’s ready."),
                        ),
                )
                .child(
                    div()
                        .w_80()
                        .max_w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            Progress::new("startup-progress")
                                .small()
                                .loading(true)
                                .accessibility_label("Preparing your file index"),
                        )
                        .child(
                            div()
                                .text_center()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(stage),
                        ),
                )
                .into_any_element()
        });
        let results = self.results.clone();
        let selected = self.selected;
        let owner = cx.entity().downgrade();
        let action_context = self.results_focus.clone();
        let no_results = self.results.is_empty();
        let preparing_index =
            !self.roots.is_empty() && self.error.is_none() && self.snapshot.scanning;
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
                "Making your files searchable. You can keep typing while we scan.",
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
            .on_action(cx.listener(|this, _: &RebuildIndex, _, cx| this.rebuild(cx)))
            .on_action(cx.listener(|this, _: &OpenResult, window, cx| this.open(false, window, cx)))
            .on_action(cx.listener(|this, _: &RevealResult, window, cx| this.open(true, window, cx)))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &NextResult, window, cx| this.move_selection(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousResult, window, cx| this.move_selection(-1, window, cx)))
            .on_action(cx.listener(|_, _: &SearchHelp, window, cx| crate::settings_view::show_error(
                "Search filenames as you type.\n\nreport budget — both words\n\"annual report\" — exact phrase\n*.pdf | *.docx — either pattern\n!backup — exclude a word\n<cat | dog> photo — group terms\next:jpg;png — extension filter\nfile: / folder: — kind\npath:Projects — search full path\nsize:>10mb — size filter\n\nAa: case sensitive. Path: match full paths. Regex: treat the entire query as a regular expression.\n\n⌘F Search · ↓/↑ Select · Space Quick Look · Return Open · ⌘Return Reveal · ⇧⌘C Copy path · ⌘R Rebuild · ⌘, Folders".into(), window, cx)))
            .child(div().flex().items_center().gap_2().px_5().pt_4().pb_3()
                .child(div().flex_1().min_w_0().child(Input::new(&self.input).id("search-input").aria_label("Search files and folders")
                .prefix(Icon::new(IconName::Search).small().text_color(cx.theme().muted_foreground))
                .suffix(div().text_xs().text_color(cx.theme().muted_foreground).child("⌘ F"))
                .cleanable(true)))
                .child(Button::new("help").ghost().icon(IconName::CircleQuestionMark).accessibility_label("Search help").tooltip("Search syntax and keyboard shortcuts").on_click(|_, window, cx| window.dispatch_action(Box::new(SearchHelp), cx))))
            .child(div().flex().flex_wrap().items_center().justify_between().px_5().pb_3().gap_2()
                .child(div().flex().items_center().gap_1().children([
                    ("Everything", IconName::Layers), ("Files", IconName::File), ("Folders", IconName::Folder),
                    ("Audio", IconName::Music2), ("Images", IconName::Image), ("Videos", IconName::Video), ("Documents", IconName::FileText)
                ].into_iter().enumerate().map(|(filter, (label, icon))| {
                    Button::new(("filter", filter)).ghost().small().icon(icon).label(label).selected(self.filter == filter)
                        .when(self.filter == filter, |this| this.bg(cx.theme().selection).text_color(cx.theme().link))
                        .on_click(cx.listener(move |this, _, window, cx| this.change_filter(filter, window, cx)))
                })))
                .child(div().flex().items_center().gap_1()
                    .child(Button::new("case").ghost().small().label("Aa").tooltip("Case sensitive").selected(self.case).when(self.case, |this| this.bg(cx.theme().selection).text_color(cx.theme().link)).on_click(cx.listener(|this, _, window, cx| { this.case = !this.case; this.search(window, cx, true); })))
                    .child(Button::new("path").ghost().small().label("Path").tooltip("Search full paths").selected(self.path).when(self.path, |this| this.bg(cx.theme().selection).text_color(cx.theme().link)).on_click(cx.listener(|this, _, window, cx| { this.path = !this.path; this.search(window, cx, true); })))
                    .child(Button::new("regex").ghost().small().label("Regex").tooltip("Use a regular expression").selected(self.regex).when(self.regex, |this| this.bg(cx.theme().selection).text_color(cx.theme().link)).on_click(cx.listener(|this, _, window, cx| { this.regex = !this.regex; this.search(window, cx, true); })))))
            .children(self.error.clone().map(|error| div().id("search-error").test_support().role(Role::Status).aria_label(error.clone())
                .mx_5().mb_3().px_3().py_2().rounded_md().bg(cx.theme().danger.opacity(0.08)).flex().items_center().gap_2().text_sm().text_color(cx.theme().danger)
                .child(Icon::new(IconName::CircleAlert).small().flex_shrink_0()).child(div().flex_1().min_w_0().child(error))))
            .child(div().flex().items_center().px_5().border_y_1().border_color(cx.theme().border).bg(cx.theme().muted).text_color(cx.theme().muted_foreground)
                .child(self.heading("Name", Sort::Name, 280., cx))
                .child(div().flex_1().min_w_0().child(self.heading("Path", Sort::Path, 140., cx)))
                .child(self.heading("Size", Sort::Size, 90., cx))
                .child(div().w(px(90.)).text_sm().py_2().child("Type"))
                .child(self.heading("Modified", Sort::Modified, 110., cx)))
            .child(div().id("results-table").test_support().track_focus(&self.results_focus).key_context("FileResults")
                .on_action(cx.listener(|this, _: &QuickLook, window, cx| this.quick_look(window, cx)))
                .role(Role::Table).aria_label(format!("{} results", self.results.len())).aria_row_count(self.results.len()).relative().flex_1().min_h_0().bg(cx.theme().table).child(
                uniform_list("results", results.len(), move |range, _, cx| {
                    range.map(|i| {
                        let entry = &results[i];
                        let name = entry.path.file_name().unwrap_or(entry.path.as_os_str()).to_string_lossy().into_owned();
                        let parent = entry.path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                        let kind = if entry.is_dir { "Folder".into() } else { entry.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_else(|| "File".into()) };
                        let owner = owner.clone();
                        let click_owner = owner.clone();
                        let mouse_owner = owner.clone();
                        let action_context = action_context.clone();
                        let menu_path = entry.path.clone();
                        div().id(("result", i)).test_support().role(Role::Row).aria_label(entry.path.to_string_lossy().into_owned())
                            .aria_selected(selected == Some(i)).flex().items_center().h(px(36.)).px_5().text_sm().w_full().cursor_pointer()
                            .when(i % 2 == 1, |row| row.bg(cx.theme().table_even))
                            .hover(|row| row.bg(if selected == Some(i) { cx.theme().selection } else { cx.theme().table_hover }))
                            .when(selected == Some(i), |row| row.bg(cx.theme().selection))
                            .child(div().w(px(280.)).flex_shrink_0().pr_3().flex().items_center().gap_2()
                                .child(Icon::new(if entry.is_dir { IconName::Folder } else { IconName::File }).small().flex_shrink_0()
                                    .text_color(if entry.is_dir { cx.theme().link } else { cx.theme().muted_foreground }))
                                .child(div().min_w_0().truncate().child(name)))
                            .child(div().flex_1().min_w_0().pr_3().truncate().text_color(cx.theme().muted_foreground).child(parent))
                            .child(div().w(px(90.)).flex_shrink_0().child(if entry.is_dir { "—".into() } else { format_size(entry.allocated_size) }))
                            .child(div().w(px(90.)).flex_shrink_0().truncate().child(kind))
                            .child(div().w(px(110.)).flex_shrink_0().child(format_modified(entry.modified)))
                            .capture_any_mouse_down(move |_, window, cx| {
                                let _ = mouse_owner.update(cx, |this, cx| this.select_result(i, window, cx));
                            })
                            .on_click(move |event, window, cx| {
                                let _ = click_owner.update(cx, |this, cx| {
                                    this.select_result(i, window, cx);
                                    if event.click_count() == 2 { this.open(false, window, cx); }
                                });
                            })
                            .context_menu(move |menu, window, cx| {
                                let dismiss_owner = owner.clone();
                                // The menu’s temporary trigger loses its focus path when dismissed.
                                cx.subscribe_in(&cx.entity(), window, move |_, _, _: &DismissEvent, window, cx| {
                                    let _ = dismiss_owner.update(cx, |this, cx| {
                                        if !this.input.focus_handle(cx).is_focused(window) && !window.has_active_dialog(cx) {
                                            this.results_focus.focus(window, cx);
                                        }
                                    });
                                }).detach();
                                let trash_owner = owner.clone();
                                let trash_path = menu_path.clone();
                                let info_owner = owner.clone();
                                let info_path = menu_path.clone();
                                let copy_owner = owner.clone();
                                let copy_path = menu_path.clone();
                                let rename_path = menu_path.clone();
                                menu.action_context(action_context.clone())
                                    .menu_with_icon("Open", IconName::ExternalLink, Box::new(OpenResult))
                                    .menu_with_icon("Show in Finder", IconName::FolderOpen, Box::new(RevealResult))
                                    .item(PopupMenuItem::new("Get Info").icon(IconName::Info)
                                        .on_click(move |_, window, cx| {
                                            let _ = info_owner.update(cx, |this, cx| this.run_command(
                                                crate::file_actions::get_info_command(&info_path), "get file info", window, cx));
                                        }))
                                    .separator()
                                    .item(PopupMenuItem::new("Copy").icon(IconName::Copy)
                                        .on_click(move |_, _, cx| {
                                            if let Err(error) = crate::file_actions::copy_file(&copy_path, &objc2_app_kit::NSPasteboard::generalPasteboard()) {
                                                let _ = copy_owner.update(cx, |this, cx| {
                                                    this.error = Some(format!("Couldn’t copy file: {error}"));
                                                    cx.notify();
                                                });
                                            }
                                        }))
                                    .menu_with_icon("Copy path", IconName::Copy, Box::new(CopyPath))
                                    .item(PopupMenuItem::new("Rename…").icon(IconName::Pencil)
                                        .on_click(move |_, window, cx| {
                                            let path = rename_path.clone();
                                            window.defer(cx, move |window, cx| crate::file_actions::show_rename(path, window, cx));
                                        }))
                                    .separator()
                                    .item(PopupMenuItem::new("Move to Trash").icon(IconName::Trash)
                                        .on_click(move |_, window, cx| {
                                            let _ = trash_owner.update(cx, |this, cx| this.trash(trash_path.clone(), window, cx));
                                        }))
                            }).into_any_element()
                    }).collect()
                }).size_full().track_scroll(&self.scroll)
            ).child(Scrollbar::vertical(&self.scroll))
                .children(startup_loading)
                .when(no_results && !self.starting, |this| this.child(div().absolute().inset_0().flex().flex_col().gap_3().items_center().justify_center().px_5()
                    .child(div().flex().items_center().justify_center().size_12().rounded_lg().bg(cx.theme().muted)
                        .when(preparing_index, |this| this.bg(cx.theme().selection))
                        .child(Icon::new(empty_icon).size_6().text_color(if preparing_index { cx.theme().link } else { cx.theme().muted_foreground })))
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(empty_title))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(empty_description))
                    .when(preparing_index, |this| this.child(div().w_72().max_w_full().mt_2().flex().flex_col().gap_2()
                        .child(Progress::new("index-progress").small().loading(true).accessibility_label("Preparing your file index"))
                        .child(div().text_center().text_xs().text_color(cx.theme().muted_foreground).child("Scanning files and folders…"))))
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
                .child(div().flex().flex_1().items_center().gap_2()
                .child(Button::new("open").primary().small().icon(IconName::ExternalLink).label("Open").tooltip("Open selected result · Return").disabled(self.selected.is_none()).on_click(cx.listener(|this, _, window, cx| this.open(false, window, cx))))
                .child(Button::new("reveal").ghost().small().icon(IconName::FolderOpen).label("Show in Finder").tooltip("Reveal selected result · ⌘Return").disabled(self.selected.is_none()).on_click(cx.listener(|this, _, window, cx| this.open(true, window, cx))))
                .child(Button::new("copy-path").ghost().small().icon(IconName::Copy).label("Copy path").tooltip("Copy full path · ⇧⌘C").disabled(self.selected.is_none()).on_click(cx.listener(|this, _, _, cx| this.copy(cx)))))
                .child(div().id("result-count").test_support().role(Role::Status).aria_label(format!("{} matches", self.results.len()))
                    .flex_shrink_0().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} matches", format_count(self.results.len()))))
                .child(div().flex().flex_1().items_center().justify_end().gap_2()
                .child(Button::new("full-disk-access").ghost().small().icon(IconName::TriangleAlert).accessibility_label("Needs Full Disk Access")
                    .tooltip(self.snapshot.scan_warning.clone().unwrap_or_else(|| "Open Full Disk Access in System Settings".into()))
                    .on_click(|_, _, cx| cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")))
                .child(div().id("navigation-hints").test_support().text_xs().text_color(cx.theme().muted_foreground).child("↑ ↓ Navigate    Space Quick Look    ↵ Open    ⌘ ↵ Reveal"))))
            .children(self.snapshot.warning.clone().map(|warning| div().id("index-warnings").test_support().role(Role::Status).aria_label(warning.clone()).max_h(px(80.)).overflow_y_scroll().px_5().pb_2().bg(cx.theme().status_bar).text_xs().text_color(cx.theme().muted_foreground).child(warning)))
            .into_any_element()
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

fn move_to_trash(path: &std::path::Path) -> Result<Option<PathBuf>, String> {
    use objc2_foundation::{NSFileManager, NSURL};

    let url = NSURL::from_file_path(path).ok_or("Invalid file path")?;
    let mut destination = None;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut destination))
        .map_err(|error| error.to_string())?;
    Ok(destination.and_then(|url| url.to_file_path()))
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
    use super::{SearchView, format_count, format_size, kind_matches, move_to_trash};
    use crate::{index::Entry, settings::Settings};
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use gpui_kit::{AppContext, ScrollStrategy, TestAppContext};
    use std::{path::PathBuf, sync::Arc};

    #[gpui_kit::test]
    async fn startup_hides_partial_results_until_the_complete_search_finishes(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.set_global(Settings {
                roots: vec![PathBuf::from("/fixture")],
            });
        });
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1050.), gpui_kit::px(450.)),
            |window, cx| {
                window.set_scroll_layers(false);
                SearchView::new(None, window, cx)
            },
        );
        let entries: Vec<_> = (0..1500)
            .map(|i| {
                Arc::new(Entry {
                    path: PathBuf::from(format!("/fixture/file-{i:04}.txt")),
                    is_dir: i == 0,
                    allocated_size: 0,
                    size: 1,
                    modified: None,
                })
            })
            .collect();
        window
            .update(cx, |view, window, cx| {
                view._poll = gpui_kit::Task::ready(());
                view.search_task = None;
                view.starting = true;
                let mut snapshot = view.snapshot.clone();
                snapshot.scanning = true;
                snapshot.loaded = false;
                snapshot.revision += 1;
                snapshot.entries = Arc::new(entries[..1024].to_vec());
                view.update_snapshot(snapshot, window, cx);
                assert!(
                    view.search_task.is_none(),
                    "Do not search a partial startup snapshot"
                );
            })
            .unwrap();
        for mode in [
            gpui_kit::component::ThemeMode::Light,
            gpui_kit::component::ThemeMode::Dark,
        ] {
            cx.update(|cx| gpui_kit::component::Theme::change(mode, None, cx));
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                let page = window.find("startup-loading");
                let progress = window.find("startup-progress");
                assert_eq!(page.label(), Some("Loading files and folders…"));
                assert!(progress.visible());
                assert_eq!(progress.bounds().center().x, page.bounds().center().x);
                assert!(progress.bounds().bottom() <= page.bounds().bottom());
                assert_eq!(page.bounds(), window.find("results-table").bounds());
                assert_eq!(window.find("results-table").label(), Some("0 results"));
                assert!(window.try_find(("result", 0usize)).is_none());
                assert!(window.find("result-count").visible());
                assert!(window.find("search-input").visible());
                assert!(window.find("Name").visible());
                assert!(window.find("open").visible());
            })
            .unwrap();
        }
        cx.update_window(window.into(), |_, window, cx| {
            window.click("search-input", cx);
            window.input("file-", cx);
            window.click(("filter", 1usize), cx);
            assert_eq!(window.find("search-input").value(), Some("file-"));
            assert!(window.find("startup-progress").visible());
        })
        .unwrap();
        let task = window
            .update(cx, |view, window, cx| {
                let mut snapshot = view.snapshot.clone();
                snapshot.entries = Arc::new(entries);
                snapshot.loaded = true;
                snapshot.revision += 1;
                view.update_snapshot(snapshot, window, cx);
                view.search_task.take().unwrap()
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("startup-loading").label(),
                Some("Preparing your search…")
            );
            assert_eq!(window.find("results-table").label(), Some("0 results"));
        })
        .unwrap();
        task.await;
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("startup-loading").is_none());
            assert_eq!(window.find("results-table").label(), Some("1499 results"));
            assert_eq!(window.find("search-input").value(), Some("file-"));
        })
        .unwrap();
        window
            .update(cx, |view, _, _| {
                assert!(
                    view.snapshot.scanning,
                    "Background checks must not delay cached results"
                );
            })
            .unwrap();
        window
            .update(cx, |view, _, cx| {
                view.snapshot.scanning = true;
                cx.notify();
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("startup-loading").is_none());
            assert_eq!(window.find("results-table").label(), Some("1499 results"));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn live_updates_finish_search_before_refreshing_and_use_the_latest_query(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.set_global(Settings { roots: Vec::new() });
        });
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1050.), gpui_kit::px(450.)),
            |window, cx| {
                window.set_scroll_layers(false);
                SearchView::new(None, window, cx)
            },
        );
        let first = window
            .update(cx, |view, window, cx| {
                view._poll = gpui_kit::Task::ready(());
                view.search_task = None;
                view.snapshot.entries = Arc::new(
                    (0..2000)
                        .map(|i| {
                            Arc::new(Entry {
                                path: PathBuf::from(format!("/fixture/dir-{i:04}/Report.txt")),
                                is_dir: false,
                                allocated_size: 0,
                                size: 1,
                                modified: None,
                            })
                        })
                        .collect(),
                );
                view.search(window, cx, true);
                let generation = view.generation.load(std::sync::atomic::Ordering::Relaxed);
                for _ in 0..10 {
                    let mut snapshot = view.snapshot.clone();
                    snapshot.revision += 1;
                    view.update_snapshot(snapshot, window, cx);
                }
                assert_eq!(
                    view.generation.load(std::sync::atomic::Ordering::Relaxed),
                    generation
                );
                view.search_task.take().unwrap()
            })
            .unwrap();
        first.await;
        window
            .update(cx, |view, _, _| {
                assert_eq!(
                    view.results.len(),
                    2000,
                    "Live updates must not starve results"
                );
                assert!(
                    view.searching,
                    "Refresh the newer snapshot after displaying results"
                );
                assert_eq!(
                    view.results[0].path,
                    PathBuf::from("/fixture/dir-0000/Report.txt")
                );
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("search-input", cx);
            window.input("missing", cx);
        })
        .unwrap();
        let latest = window
            .update(cx, |view, _, _| view.search_task.take().unwrap())
            .unwrap();
        latest.await;
        window
            .update(cx, |view, _, _| {
                assert!(!view.searching);
                assert!(view.results.is_empty());
            })
            .unwrap();
    }

    #[gpui_kit::test]
    fn first_scan_shows_progress_and_keeps_search_available(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.set_global(Settings { roots: Vec::new() });
        });
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1050.), gpui_kit::px(450.)),
            |window, cx| {
                window.set_scroll_layers(false);
                SearchView::new(None, window, cx)
            },
        );
        window
            .update(cx, |view, _, cx| {
                view._poll = gpui_kit::Task::ready(());
                view.search_task = None;
                view.roots = vec![PathBuf::from("/fixture")];
                view.snapshot.scanning = true;
                cx.notify();
            })
            .unwrap();
        for mode in [
            gpui_kit::component::ThemeMode::Light,
            gpui_kit::component::ThemeMode::Dark,
        ] {
            cx.update(|cx| gpui_kit::component::Theme::change(mode, None, cx));
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                let progress = window.find("index-progress");
                assert_eq!(progress.label(), Some("Preparing your file index"));
                assert_eq!(progress.role(), Some(gpui_kit::Role::ProgressIndicator));
                assert!(progress.visible());
                let bounds = progress.bounds();
                let table = window.find("results-table").bounds();
                assert!(bounds.size.width > gpui_kit::px(0.));
                assert!(bounds.origin.y >= table.origin.y && bounds.bottom() <= table.bottom());
                assert_eq!(bounds.center().x, table.center().x);
            })
            .unwrap();
        }
        cx.update_window(window.into(), |_, window, cx| {
            window.click("search-input", cx);
            window.input("report", cx);
            assert_eq!(window.find("search-input").value(), Some("report"));
            assert!(window.find("index-progress").visible());
        })
        .unwrap();
        window
            .update(cx, |view, _, cx| {
                view.snapshot.scanning = false;
                cx.notify();
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("index-progress").is_none());
            assert_eq!(window.find("search-input").value(), Some("report"));
        })
        .unwrap();
    }

    #[test]
    fn trash_preserves_contents_and_symlink_targets_and_reports_invalid_paths() {
        use std::{
            fs,
            os::unix::fs::symlink,
            time::{SystemTime, UNIX_EPOCH},
        };

        let directory = std::env::temp_dir().join(format!(
            "everything-trash-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let file = directory.join("a 'quoted' 雪; file.txt");
        let folder = directory.join("folder");
        let link = directory.join("link");
        fs::write(&file, b"keep this content").unwrap();
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("nested.txt"), b"nested content").unwrap();
        symlink(&file, &link).unwrap();
        for path in [&link, &file, &folder] {
            let destination = move_to_trash(path).unwrap().unwrap();
            let original_missing = fs::symlink_metadata(path).is_err();
            fs::rename(destination, path).unwrap();
            assert!(original_missing);
            assert_eq!(fs::read(&file).unwrap(), b"keep this content");
        }
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read(folder.join("nested.txt")).unwrap(),
            b"nested content"
        );
        assert!(move_to_trash(&directory.join("missing")).is_err());
        assert!(move_to_trash(&directory.join("invalid\0path")).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[gpui_kit::test]
    async fn rebuild_cache_is_in_settings_and_rescans(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (handle, view) = cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.set_global(Settings { roots: Vec::new() });
            let (handle, view) =
                gpui_kit::open_window(gpui_kit::WindowOptions::default(), cx, |window, cx| {
                    window.set_scroll_layers(false);
                    cx.new(|cx| SearchView::new(None, window, cx))
                })
                .unwrap();
            crate::settings_view::register_action(
                handle,
                view.clone(),
                Some(PathBuf::from("unused-settings.json")),
                None,
                cx,
            );
            (handle, view)
        });
        cx.wait_for(handle, std::time::Duration::from_secs(3), |_, cx| {
            std::thread::sleep(std::time::Duration::from_millis(2));
            !view.read(cx).is_scanning()
        })
        .await;
        let scans = cx.update(|cx| view.read(cx).index.snapshot().full_scans);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("rebuild").is_none());
            assert!(window.try_find("folders").is_none());
            window.dispatch_action(Box::new(crate::OpenSettings), cx);
        })
        .unwrap();
        cx.wait_for(handle, std::time::Duration::from_secs(3), |window, _| {
            window.within("dialog").try_find("rebuild").is_some()
        })
        .await;
        cx.update_window(handle, |_, window, cx| {
            assert_eq!(
                window.within("dialog").find("rebuild").label(),
                Some("Rebuild cache")
            );
            window.within("dialog").click("rebuild", cx);
            assert!(window.try_find("dialog").is_some());
        })
        .unwrap();
        cx.wait_for(handle, std::time::Duration::from_secs(3), |_, cx| {
            std::thread::sleep(std::time::Duration::from_millis(2));
            view.read(cx).index.snapshot().full_scans > scans
        })
        .await;
    }

    #[gpui_kit::test]
    fn access_icon_stays_left_of_navigation_and_opens_full_disk_access(cx: &mut TestAppContext) {
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
                let icon = window.find("full-disk-access").bounds();
                let navigation = window.find("navigation-hints").bounds();
                assert!(icon.right() <= navigation.origin.x);
                assert!(icon.origin.y <= navigation.origin.y);
                assert!(icon.bottom() >= navigation.bottom());
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
    async fn tabs_restore_their_previous_order_without_sorting(cx: &mut TestAppContext) {
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
        let view = window
            .update(cx, |view, window, cx| {
                view._poll = gpui_kit::Task::ready(());
                view.search_task = None;
                view.snapshot.entries = Arc::new(
                    (0..500)
                        .map(|i| {
                            Arc::new(Entry {
                                path: PathBuf::from(format!(
                                    "/fixture/file-{i:03}.{}",
                                    if i % 50 == 49 { "mp4" } else { "txt" }
                                )),
                                is_dir: false,
                                allocated_size: i as u64 * 4096,
                                size: i as u64,
                                modified: Some(
                                    std::time::SystemTime::UNIX_EPOCH
                                        + std::time::Duration::from_secs(i),
                                ),
                            })
                        })
                        .collect(),
                );
                view.search(window, cx, true);
                cx.entity()
            })
            .unwrap();
        cx.wait_for(window.into(), std::time::Duration::from_secs(3), |_, cx| {
            !view.read(cx).searching && !view.read(cx).sorting
        })
        .await;
        let everything_results = cx.update(|cx| view.read(cx).results.clone());
        let unexpected_sorts = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = unexpected_sorts.clone();
        let _subscription = cx.update(|cx| {
            cx.observe(&view, move |view, cx| {
                if view.read(cx).sorting && view.read(cx).filter == 0 {
                    observed.set(observed.get() + 1);
                }
            })
        });
        window
            .update(cx, |view, _, cx| {
                view.sorting = true;
                // A filter must not wait even when the previous sort never finishes.
                view.search_task = Some(cx.spawn(async |_, _| std::future::pending().await));
            })
            .unwrap();
        for (step, heading) in [
            "Name", "Name", "Size", "Size", "Modified", "Modified", "Path", "Path",
        ]
        .into_iter()
        .enumerate()
        {
            cx.update_window(window.into(), |_, window, cx| {
                window.click(("filter", 5usize), cx)
            })
            .unwrap();
            cx.wait_for(window.into(), std::time::Duration::from_secs(3), |_, cx| {
                !view.read(cx).searching && !view.read(cx).sorting
            })
            .await;
            cx.update_window(window.into(), |_, window, _| {
                assert_eq!(window.find("results-table").label(), Some("10 results"));
            })
            .unwrap();
            cx.update_window(window.into(), |_, window, cx| window.click(heading, cx))
                .unwrap();
            if step % 2 == 0 {
                cx.wait_for(window.into(), std::time::Duration::from_secs(3), |_, cx| {
                    !view.read(cx).searching && !view.read(cx).sorting
                })
                .await;
            }
            cx.update_window(window.into(), |_, window, cx| {
                window.click(("filter", 0usize), cx)
            })
            .unwrap();
            cx.wait_for(
                window.into(),
                std::time::Duration::from_secs(3),
                |window, cx| {
                    window.find("results-table").label() == Some("500 results")
                        && !view.read(cx).sorting
                        && !view.read(cx).searching
                },
            )
            .await;
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(
                    window.try_find(("result", 0usize)).is_some(),
                    "{heading}: Everything should show the start of the sorted results"
                );
            })
            .unwrap();
            cx.update(|cx| {
                let view = view.read(cx);
                assert_eq!(view.filter, 0);
                assert!(Arc::ptr_eq(&view.results, &everything_results));
                assert!(view.search_task.is_none());
                assert!(!view.sorting);
                assert!(!view.descending);
                assert_eq!(view.results[0].path, PathBuf::from("/fixture/file-000.txt"));
            });
        }
        assert_eq!(unexpected_sorts.get(), 0, "Changing tabs must not sort");
        let (videos, sort, descending) = cx.update(|cx| {
            let saved = view.read(cx).filter_results[5].as_ref().unwrap();
            (saved.results.clone(), saved.sort, saved.descending)
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.click(("filter", 5usize), cx);
        })
        .unwrap();
        cx.update(|cx| {
            let view = view.read(cx);
            assert!(Arc::ptr_eq(&view.results, &videos));
            assert!(view.sort == sort);
            assert_eq!(view.descending, descending);
            assert!(view.search_task.is_none());
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.click("search-input", cx);
            window.input("file-049", cx);
        })
        .unwrap();
        cx.wait_for(window.into(), std::time::Duration::from_secs(3), |_, cx| {
            !view.read(cx).searching && !view.read(cx).sorting
        })
        .await;
        cx.update_window(window.into(), |_, window, cx| {
            window.click(("filter", 0usize), cx);
        })
        .unwrap();
        cx.wait_for(
            window.into(),
            std::time::Duration::from_secs(3),
            |window, cx| {
                window.find("results-table").label() == Some("1 results")
                    && !view.read(cx).searching
            },
        )
        .await;
        let task = window
            .update(cx, |view, window, cx| {
                let mut snapshot = view.snapshot.clone();
                snapshot.revision += 1;
                snapshot.entries = Arc::new(
                    snapshot
                        .entries
                        .iter()
                        .filter(|entry| entry.path != PathBuf::from("/fixture/file-049.mp4"))
                        .cloned()
                        .collect(),
                );
                view.update_snapshot(snapshot, window, cx);
                view.search_task.take().unwrap()
            })
            .unwrap();
        task.await;
        cx.update_window(window.into(), |_, window, cx| {
            window.click(("filter", 5usize), cx);
        })
        .unwrap();
        cx.wait_for(
            window.into(),
            std::time::Duration::from_secs(3),
            |window, _| window.find("results-table").label() == Some("0 results"),
        )
        .await;
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
                view.search_task = None;
                view.snapshot.entries = Arc::new(
                    (0..500)
                        .map(|i| {
                            Arc::new(Entry {
                                path: PathBuf::from(format!("/fixture/file-{i:03}.txt")),
                                is_dir: false,
                                allocated_size: i as u64 * 4096,
                                size: (500 - i) as u64,
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
                    allocated_size: 0,
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
            allocated_size: 0,
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
