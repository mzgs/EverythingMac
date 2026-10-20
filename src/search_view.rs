use crate::{
    content_search,
    index::{Entry, Index, Snapshot},
    query::Query,
    saved_searches::{SavedSearch, SavedSearches, Sort},
    search_history::SearchHistory,
    settings::Settings,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::actions::Confirm;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Icon, Selectable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        input::{Input, InputEvent, InputState},
        menu::{ContextMenuExt, PopupMenuItem},
        popover::Popover,
        progress::Progress,
        scroll::Scrollbar,
        spinner::Spinner,
        tooltip::Tooltip,
    },
    *,
};
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

const RESULT_ROW_HEIGHT: Rems = rems(1.5);

actions!(
    everything,
    [
        FocusSearch,
        RebuildIndex,
        OpenResult,
        RevealResult,
        CopyPath,
        CopyFiles,
        TrashResults,
        SelectAllResults,
        ExtendNextResult,
        ExtendPreviousResult,
        QuickLook,
        NextResult,
        PreviousResult,
        SearchHelp
    ]
);

enum SearchQuery {
    Filename(Query),
    Content(Option<String>),
}

impl SearchQuery {
    fn parse(
        text: &str,
        case: bool,
        path: bool,
        regex: bool,
        content: bool,
    ) -> Result<Self, String> {
        if content {
            content_search::predicate(text, case).map(Self::Content)
        } else {
            Query::parse(text, case, path, regex, !path && !regex).map(Self::Filename)
        }
    }
}

struct FilterResults {
    results: Arc<Vec<Arc<Entry>>>,
    selected: Option<usize>,
    selected_paths: Arc<BTreeSet<PathBuf>>,
    selection_anchor: Option<PathBuf>,
    sort: Sort,
    descending: bool,
}

pub(crate) struct SearchView {
    input: Entity<InputState>,
    history: SearchHistory,
    history_open: bool,
    history_query: SharedString,
    history_task: Option<Task<()>>,
    saved: SavedSearches,
    saved_open: bool,
    saved_name: Entity<InputState>,
    results_focus: FocusHandle,
    preview: Option<crate::quick_look::Preview>,
    index: Index,
    roots: Vec<PathBuf>,
    excluded: Vec<PathBuf>,
    database_path: Option<PathBuf>,
    snapshot: Snapshot,
    results: Arc<Vec<Arc<Entry>>>,
    selected: Option<usize>,
    selected_paths: Arc<BTreeSet<PathBuf>>,
    selection_anchor: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    case: bool,
    path: bool,
    regex: bool,
    content: bool,
    scope: Option<PathBuf>,
    search_day: chrono::NaiveDate,
    filter: usize,
    filter_results: [Option<FilterResults>; 7],
    sort: Sort,
    descending: bool,
    searching: bool,
    sorting: bool,
    finding_duplicates: bool,
    duplicates: Option<Arc<crate::duplicates::Report>>,
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
        let excluded = cx.global::<Settings>().excluded.clone();
        let starting = !roots.is_empty();
        let index = Index::start(roots.clone(), database_path.clone(), excluded.clone());
        let snapshot = index.snapshot();
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search files and folders…"));
        let saved_name = cx.new(|cx| InputState::new(window, cx).placeholder("Name this search…"));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                InputEvent::Change => {
                    this.search(window, cx, true);
                    this.history_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_secs(1)).await;
                        let _ = this.update(cx, |this, cx| this.remember_search(cx));
                    }));
                }
                InputEvent::Blur => this.remember_search(cx),
                InputEvent::PressEnter { secondary, .. } => this.open(*secondary, window, cx),
                _ => {}
            });
        cx.on_app_quit(|this, cx| {
            this.remember_search(cx);
            async {}
        })
        .detach();
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| {
                        let roots = cx.global::<Settings>().roots.clone();
                        let excluded = cx.global::<Settings>().excluded.clone();
                        if roots != this.roots || excluded != this.excluded {
                            this.roots = roots.clone();
                            this.excluded = excluded.clone();
                            this.index.stop();
                            this.index = Index::start(roots, this.database_path.clone(), excluded);
                            this.snapshot = this.index.snapshot();
                            this.search(window, cx, true);
                        }
                        let snapshot = this.index.snapshot();
                        this.update_snapshot(snapshot, window, cx);
                        if this.search_day != chrono::Local::now().date_naive() {
                            this.filter_results.fill_with(|| None);
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
            history: SearchHistory::load(
                database_path
                    .as_ref()
                    .map(|path| path.with_file_name("search-history.json")),
            ),
            history_open: false,
            history_query: "".into(),
            history_task: None,
            saved: SavedSearches::load(
                database_path
                    .as_ref()
                    .map(|path| path.with_file_name("saved-searches.json")),
            ),
            saved_open: false,
            saved_name,
            results_focus: cx.focus_handle().tab_stop(true),
            preview: None,
            index,
            roots,
            excluded,
            database_path,
            snapshot,
            results: Arc::new(Vec::new()),
            selected: None,
            selected_paths: Arc::new(BTreeSet::new()),
            selection_anchor: None,
            scroll: UniformListScrollHandle::new(),
            case: false,
            path: false,
            regex: false,
            content: false,
            scope: None,
            search_day: chrono::Local::now().date_naive(),
            filter: 0,
            filter_results: std::array::from_fn(|_| None),
            sort: Sort::BestMatch,
            descending: false,
            searching: false,
            sorting: false,
            finding_duplicates: false,
            duplicates: None,
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

    fn remember_search(&mut self, cx: &mut Context<Self>) {
        self.history_task = None;
        let query = self.input.read(cx).value();
        if query != self.history_query && self.parse_query(&query).is_ok() {
            self.history.remember(&query);
            self.history_query = query;
            cx.notify();
        }
    }

    fn parse_query(&self, text: &str) -> Result<SearchQuery, String> {
        SearchQuery::parse(text, self.case, self.path, self.regex, self.content)
    }

    fn save_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        if let Err(error) = self.parse_query(&query) {
            self.saved.error = Some(error);
        } else if self.saved.add(SavedSearch {
            name: self.saved_name.read(cx).value().to_string(),
            query,
            case: self.case,
            path: self.path,
            regex: self.regex,
            content: self.content,
            fuzzy: true,
            scope: self.scope.clone(),
            filter: self.filter,
            sort: self.sort,
            descending: self.descending,
        }) {
            self.saved_open = false;
            self.saved_name
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    fn recall_saved(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(saved) = self.saved.entries.get(index).cloned() else {
            return;
        };
        self.case = saved.case;
        self.path = saved.path;
        self.regex = saved.regex;
        self.content = saved.content;
        self.scope = saved.scope;
        self.filter = saved.filter;
        self.sort = saved.sort;
        self.descending = saved.descending;
        self.saved_open = false;
        self.input.update(cx, |input, cx| {
            input.set_value(saved.query, window, cx);
            input.focus(window, cx);
        });
        self.search(window, cx, true);
    }

    fn saved_searches(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Popover::new("saved-searches-popover")
            .open(self.saved_open)
            .on_open_change(cx.listener(|this, open, _, cx| {
                this.saved_open = *open;
                cx.notify();
            }))
            .trigger(
                Button::new("saved-searches-toggle")
                    .ghost()
                    .small()
                    .icon(IconName::Bookmark)
                    .accessibility_label("Saved searches")
                    .tooltip("Save or recall a complete search"),
            )
            .child(
                div()
                    .id("saved-searches")
                    .test_support()
                    .w(px(360.))
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Saved searches"),
                    )
                    .child(
                        Input::new(&self.saved_name)
                            .id("saved-search-name")
                            .aria_label("Saved search name"),
                    )
                    .child(
                        Button::new("save-search")
                            .label("Save current search")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save_search(window, cx)),
                            )
                            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                                this.save_search(window, cx)
                            })),
                    )
                    .child(
                        div()
                            .id("saved-search-entries")
                            .max_h(px(280.))
                            .overflow_y_scroll()
                            .when(self.saved.entries.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No saved searches"),
                                )
                            })
                            .children(self.saved.entries.iter().enumerate().map(
                                |(index, saved)| {
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Button::new(("saved-search", index))
                                                .ghost()
                                                .flex_1()
                                                .min_w_0()
                                                .accessibility_label(saved.name.clone())
                                                .tooltip(saved.query.clone())
                                                .child(
                                                    div()
                                                        .w_full()
                                                        .truncate()
                                                        .child(saved.name.clone()),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.recall_saved(index, window, cx)
                                                    },
                                                ))
                                                .on_action(cx.listener(
                                                    move |this, _: &Confirm, window, cx| {
                                                        this.recall_saved(index, window, cx)
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new(("remove-saved-search", index))
                                                .ghost()
                                                .small()
                                                .icon(IconName::X)
                                                .accessibility_label(format!(
                                                    "Remove saved search {}",
                                                    saved.name
                                                ))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.saved.remove(index);
                                                    cx.notify();
                                                }))
                                                .on_action(cx.listener(
                                                    move |this, _: &Confirm, _, cx| {
                                                        this.saved.remove(index);
                                                        cx.notify();
                                                    },
                                                )),
                                        )
                                },
                            )),
                    )
                    .children(self.saved.error.clone().map(|error| {
                        div()
                            .id("saved-search-error")
                            .test_support()
                            .role(Role::Status)
                            .aria_label(error.clone())
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(error)
                    })),
            )
    }

    fn set_scope(&mut self, scope: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if scope.is_some() {
            self.filter = 0;
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.scope = scope;
        self.input.update(cx, |input, cx| input.focus(window, cx));
        self.search(window, cx, true);
    }

    fn drag_paths(&self, path: &std::path::Path) -> FileDragPaths {
        let selected = self.selected_paths.contains(path);
        FileDragPaths::new(
            self.results
                .iter()
                .filter(|entry| {
                    if selected {
                        self.selected_paths.contains(&entry.path)
                    } else {
                        entry.path == path
                    }
                })
                .map(|entry| (entry.path.clone(), entry.is_dir)),
        )
    }

    fn recall_history(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.history.entries[index].clone();
        self.history_open = false;
        self.history.remember(&query);
        self.history_query = query.clone().into();
        self.input.update(cx, |input, cx| {
            input.set_value(query, window, cx);
            input.focus(window, cx);
        });
        self.search(window, cx, true);
    }

    fn search_history(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        Popover::new("search-history-popover")
            .open(self.history_open)
            .on_open_change(cx.listener(|this, open, _, cx| {
                if *open {
                    this.remember_search(cx);
                }
                this.history_open = *open;
                cx.notify();
            }))
            .trigger(
                Button::new("search-history-toggle")
                    .ghost()
                    .small()
                    .rounded_full()
                    .accessibility_label("Search history")
                    .tooltip("Search history")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::Search).small())
                            .child(Icon::new(IconName::ChevronDown).xsmall()),
                    ),
            )
            .w((window.viewport_size().width - px(100.))
                .max(px(160.))
                .min(px(480.)))
            .p_2()
            .child(
                div()
                    .id("search-history")
                    .test_support()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Search history"),
                    )
                    .child(
                        div()
                            .id("search-history-entries")
                            .max_h(px(336.))
                            .overflow_y_scroll()
                            .when(self.history.entries.is_empty(), |this| {
                                this.child(
                                    div()
                                        .px_2()
                                        .py_3()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No recent searches"),
                                )
                            })
                            .children(self.history.entries.iter().enumerate().map(
                                |(index, query)| {
                                    let query = query.clone();
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Button::new(("search-history-query", index))
                                                .ghost()
                                                .w_full()
                                                .flex_1()
                                                .min_w_0()
                                                .accessibility_label(query.clone())
                                                .tooltip(query.clone())
                                                .child(
                                                    div()
                                                        .w_full()
                                                        .min_w_0()
                                                        .truncate()
                                                        .font_family(
                                                            cx.theme().mono_font_family.clone(),
                                                        )
                                                        .child(query.clone()),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.recall_history(index, window, cx)
                                                    },
                                                ))
                                                .on_action(cx.listener(
                                                    move |this, _: &Confirm, window, cx| {
                                                        this.recall_history(index, window, cx)
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new(("search-history-remove", index))
                                                .ghost()
                                                .small()
                                                .icon(IconName::X)
                                                .accessibility_label(format!(
                                                    "Remove {} from search history",
                                                    self.history.entries[index]
                                                ))
                                                .tooltip("Remove from history")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.history.remove(index);
                                                    cx.notify();
                                                }))
                                                .on_action(cx.listener(
                                                    move |this, _: &Confirm, _, cx| {
                                                        this.history.remove(index);
                                                        cx.notify();
                                                    },
                                                )),
                                        )
                                },
                            )),
                    )
                    .children(self.history.error.clone().map(|error| {
                        div()
                            .id("search-history-error")
                            .test_support()
                            .role(Role::Status)
                            .aria_label(error.clone())
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(error)
                    })),
            )
    }

    pub(crate) fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.index.refresh();
        cx.notify();
    }

    fn update_snapshot(&mut self, snapshot: Snapshot, window: &mut Window, cx: &mut Context<Self>) {
        if snapshot.revision != self.snapshot.revision {
            self.filter_results.fill_with(|| None);
            self.snapshot = snapshot;
            if let Some(report) = &mut self.duplicates {
                let report = Arc::make_mut(report);
                report.retain_current(&self.snapshot.entries);
                let results = report.results.clone();
                self.set_results(results, cx, false);
            } else if self.search_task.is_none() && (!self.snapshot.scanning || self.starting) {
                self.refresh_results(window, cx);
            }
        }
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>, reveal_selection: bool) {
        self.search_in_order(window, cx, reveal_selection, None);
    }

    fn refresh_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = (!self.starting).then(|| self.results.clone());
        self.search_in_order(window, cx, false, previous);
    }

    fn search_in_order(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        reveal_selection: bool,
        previous: Option<Arc<Vec<Arc<Entry>>>>,
    ) {
        self.finding_duplicates = false;
        self.duplicates = None;
        self.search_day = chrono::Local::now().date_naive();
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
        let text = self.input.read(cx).value();
        let (content, case, path, regex) = (self.content, self.case, self.path, self.regex);
        self.error = None;
        self.searching = true;
        let revision = self.snapshot.revision;
        let loaded = self.snapshot.loaded;
        let entries = self.snapshot.entries.clone();
        let roots = self.roots.clone();
        let scope = self.scope.clone();
        let filter = self.filter;
        let sort = self.sort;
        let descending = self.descending;
        let preserve_scroll = previous.is_some();
        let needs_sort = previous.is_none()
            && (descending
                || match sort {
                    Sort::Path => false,
                    Sort::BestMatch => !text.trim().is_empty() && !content && !regex && !path,
                    _ => true,
                });
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
                        return Ok(None);
                    }
                    // Compile only the debounced query, away from keyboard handling.
                    let query = SearchQuery::parse(&text, case, path, regex, content)?;
                    let content_paths = match &query {
                        // shortcut: Spotlight changes refresh on the next search; subscribe if live content updates are needed.
                        SearchQuery::Content(predicate) => {
                            match content_search::search(
                                predicate.as_deref(),
                                &roots,
                                &matching_generation,
                                generation,
                            )? {
                                Some(paths) => paths,
                                None => return Ok(None),
                            }
                        }
                        SearchQuery::Filename(_) => Default::default(),
                    };
                    let mut results = Vec::new();
                    for (i, entry) in entries.iter().enumerate() {
                        if i % 1024 == 0
                            && matching_generation.load(Ordering::Relaxed) != generation
                        {
                            return Ok(None);
                        }
                        if scope.as_ref().is_none_or(|scope| {
                            entry.path != *scope && entry.path.starts_with(scope)
                        }) && kind_matches(entry, filter)
                            && match &query {
                                SearchQuery::Filename(query) => query.matches(
                                    &entry.path,
                                    entry.is_dir,
                                    entry.size,
                                    entry.modified,
                                ),
                                SearchQuery::Content(_) => {
                                    !entry.is_dir && content_paths.contains(&entry.path)
                                }
                            }
                        {
                            results.push(entry.clone());
                        }
                    }
                    if let Some(previous) = previous {
                        let ranking = match &query {
                            SearchQuery::Filename(query) if !regex && !path => Some(query),
                            _ => None,
                        };
                        results = merge_results(&previous, &results, sort, descending, ranking);
                    }
                    Ok::<_, String>(Some((Arc::new(results), query)))
                })
                .await;
            let (mut results, query) = match result {
                Ok(Some(results)) => results,
                Ok(None) => return,
                Err(error) => {
                    let _ = this.update_in(cx, |this, _, cx| {
                        if this.generation.load(Ordering::Relaxed) != generation {
                            return;
                        }
                        this.search_task = None;
                        this.searching = false;
                        this.starting = false;
                        this.pending_open = None;
                        this.error = Some(error);
                        this.set_results(Arc::new(Vec::new()), cx, reveal_selection);
                    });
                    return;
                }
            };
            if needs_sort && results.len() > 1 {
                let published = this.update_in(cx, |this, _, cx| {
                    if this.generation.load(Ordering::Relaxed) != generation {
                        return false;
                    }
                    this.searching = false;
                    this.sorting = true;
                    cx.notify();
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
                            Sort::BestMatch => {
                                if !regex
                                    && !path
                                    && let SearchQuery::Filename(query) = &query
                                {
                                    results.sort_by_cached_key(|entry| {
                                        query.rank(
                                            &entry.path,
                                            entry.is_dir,
                                            entry.size,
                                            entry.modified,
                                        )
                                    });
                                }
                            }
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
                            Sort::Size | Sort::Modified => {
                                results.sort_by(|a, b| compare_entries(a, b, sort, None))
                            }
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
                let scroll_offset = preserve_scroll
                    .then(|| {
                        let state = this.scroll.0.borrow();
                        let mut offset = state.base_handle.offset();
                        let height = RESULT_ROW_HEIGHT.to_pixels(window.rem_size());
                        if offset.y >= px(0.) {
                            return None;
                        }
                        let old_index = (-offset.y / height).floor() as usize;
                        let anchor = this.results.get(old_index)?;
                        let new_index =
                            results.iter().position(|entry| entry.path == anchor.path)?;
                        offset.y += height * (old_index as f32 - new_index as f32);
                        Some(offset)
                    })
                    .flatten();
                this.set_results(results, cx, reveal_selection);
                if let Some(offset) = scroll_offset {
                    this.scroll.0.borrow().base_handle.set_offset(offset);
                }
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
                        selected_paths: this.selected_paths.clone(),
                        selection_anchor: this.selection_anchor.clone(),
                        sort,
                        descending,
                    });
                } else if !this.snapshot.scanning {
                    this.refresh_results(window, cx);
                }
            });
        }));
        cx.notify();
    }

    pub(crate) fn duplicates_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("find-duplicates")
            .ghost()
            .small()
            .size_7()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background.opacity(0.55))
            .text_color(cx.theme().muted_foreground)
            .icon(IconName::Copy)
            .accessibility_label("Find duplicate files")
            .tooltip("Find duplicate files")
            .selected(self.finding_duplicates || self.duplicates.is_some())
            .toggled(self.finding_duplicates || self.duplicates.is_some())
            .disabled(
                self.starting
                    || self.searching
                    || self.sorting
                    || ((self.snapshot.scanning || self.error.is_some())
                        && !self.finding_duplicates
                        && self.duplicates.is_none()),
            )
            .on_click(cx.listener(|this, _, window, cx| this.find_duplicates(window, cx)))
    }

    fn find_duplicates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.finding_duplicates || self.duplicates.is_some() {
            self.search(window, cx, true);
            return;
        }
        if self.starting
            || self.searching
            || self.sorting
            || self.snapshot.scanning
            || self.error.is_some()
        {
            return;
        }
        self.filter_results.fill_with(|| None);
        self.pending_open = None;
        self.error = None;
        self.finding_duplicates = true;
        let entries = self.results.clone();
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let current = self.generation.clone();
        self.search_task = Some(cx.spawn_in(window, async move |this, cx| {
            let report = cx
                .background_spawn(
                    async move { crate::duplicates::find(&entries, &current, generation) },
                )
                .await;
            let Some(mut report) = report else { return };
            let _ = this.update_in(cx, |this, _, cx| {
                if this.generation.load(Ordering::Relaxed) != generation {
                    return;
                }
                this.search_task = None;
                this.finding_duplicates = false;
                report.retain_current(&this.snapshot.entries);
                this.set_results(report.results.clone(), cx, true);
                this.duplicates = Some(Arc::new(report));
                cx.notify();
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
            saved.selected_paths = self.selected_paths.clone();
            saved.selection_anchor = self.selection_anchor.clone();
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
            self.selected_paths = saved.selected_paths.clone();
            self.selection_anchor = saved.selection_anchor.clone();
            self.sort = saved.sort;
            self.descending = saved.descending;
            cx.notify();
        } else {
            self.sort = Sort::BestMatch;
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
        let active = self
            .selected
            .and_then(|i| self.results.get(i))
            .map(|entry| &entry.path);
        let mut paths = BTreeSet::new();
        let mut selected = None;
        if !self.selected_paths.is_empty() {
            for (i, entry) in results.iter().enumerate() {
                if self.selected_paths.contains(&entry.path) {
                    paths.insert(entry.path.clone());
                    if selected.is_none() || active == Some(&entry.path) {
                        selected = Some(i);
                    }
                    // Widening a query usually retains the selection near the top.
                    if paths.len() == self.selected_paths.len() {
                        break;
                    }
                }
            }
        }
        self.selected = selected.or_else(|| (!results.is_empty()).then_some(0));
        if paths.is_empty()
            && let Some(i) = self.selected
        {
            paths.insert(results[i].path.clone());
        }
        if self
            .selection_anchor
            .as_ref()
            .is_none_or(|path| !paths.contains(path))
        {
            self.selection_anchor = self.selected.map(|i| results[i].path.clone());
        }
        self.selected_paths = Arc::new(paths);
        self.results = results;
        if reveal_selection {
            self.scroll
                .scroll_to_item(self.selected.unwrap_or(0), ScrollStrategy::Top);
        }
        cx.notify();
    }

    fn open(&mut self, reveal: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_search(cx);
        if self.searching {
            self.pending_open = Some(reveal);
            return;
        }
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let mut command = std::process::Command::new("/usr/bin/open");
        if reveal {
            command.arg("-R");
        }
        command.args(paths);
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

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.results
            .iter()
            .filter(|entry| self.selected_paths.contains(&entry.path))
            .map(|entry| entry.path.clone())
            .collect()
    }

    fn copy(&self, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if !paths.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                paths
                    .iter()
                    .map(|path| path.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
        }
    }

    fn copy_files(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = crate::file_actions::copy_files(
            &self.selected_paths(),
            &objc2_app_kit::NSPasteboard::generalPasteboard(),
        ) {
            self.error = Some(format!("Couldn’t copy files: {error}"));
            cx.notify();
        }
    }

    fn trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { trash_paths(paths) }).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.error = Some(format!("Couldn’t move to Trash: {error}"));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selected_paths = Arc::new(
            self.results
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        );
        self.selected = self
            .selected
            .or_else(|| (!self.results.is_empty()).then_some(0));
        self.selection_anchor = self.selected.map(|i| self.results[i].path.clone());
        cx.notify();
    }

    fn select_result(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.select_with_modifiers(index, Modifiers::default(), false, window, cx);
    }

    fn select_with_modifiers(
        &mut self,
        index: usize,
        modifiers: Modifiers,
        context_menu: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.results.get(index) else {
            return;
        };
        let path = entry.path.clone();
        if context_menu && self.selected_paths.contains(&path) {
            self.selected = Some(index);
        } else if modifiers.shift && !context_menu {
            let anchor = self
                .selection_anchor
                .as_ref()
                .and_then(|path| self.results.iter().position(|entry| &entry.path == path))
                .unwrap_or(index);
            let paths = Arc::make_mut(&mut self.selected_paths);
            if !modifiers.platform {
                paths.clear();
            }
            paths.extend(
                self.results[anchor.min(index)..=anchor.max(index)]
                    .iter()
                    .map(|entry| entry.path.clone()),
            );
            self.selection_anchor = Some(self.results[anchor].path.clone());
            self.selected = Some(index);
        } else if modifiers.platform && !context_menu {
            let paths = Arc::make_mut(&mut self.selected_paths);
            if !paths.remove(&path) {
                paths.insert(path.clone());
            }
            self.selected = if paths.contains(&path) {
                Some(index)
            } else {
                self.results
                    .iter()
                    .position(|entry| paths.contains(&entry.path))
            };
            self.selection_anchor = self.selected.map(|i| self.results[i].path.clone());
        } else {
            self.selected_paths = Arc::new(BTreeSet::from([path.clone()]));
            self.selection_anchor = Some(path);
            self.selected = Some(index);
        }
        self.results_focus.focus(window, cx);
        if let Some(preview) = &self.preview
            && let Some(entry) = self.results.get(index)
            && let Err(error) = preview.update_if_visible(&entry.path)
        {
            self.error = Some(format!("Couldn’t preview result: {error}"));
        }
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

    fn move_selection(
        &mut self,
        direction: isize,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.results.is_empty() {
            return;
        }
        let index = self
            .selected
            .unwrap_or(0)
            .saturating_add_signed(direction)
            .min(self.results.len() - 1);
        if extend {
            self.select_with_modifiers(
                index,
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
                false,
                window,
                cx,
            );
        } else {
            self.select_result(index, window, cx);
        }
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
        let duplicates = self.duplicates.is_some();
        let selected = if duplicates {
            sort == Sort::Size
        } else {
            self.sort == sort
        };
        Button::new(label)
            .ghost()
            .small()
            .disabled(self.finding_duplicates || self.duplicates.is_some())
            .accessibility_label(label)
            .tooltip(if duplicates && sort == Sort::Size {
                "Duplicate groups ordered by logical file size, largest first".to_string()
            } else if sort == Sort::Size {
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
                    .when(selected, |this| {
                        this.child(if self.sorting {
                            Spinner::new().xsmall().into_any_element()
                        } else {
                            Icon::new(if duplicates || self.descending {
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

impl Drop for SearchView {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl Render for SearchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let duplicates = self.duplicates.clone();
        let selected_paths = self.selected_paths.clone();
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
                if self.content {
                    "Searching file contents…"
                } else {
                    "Searching your index…"
                },
                "Finding files and folders that match your search.",
            )
        } else if self.duplicates.is_some() {
            (
                IconName::SearchX,
                "No duplicates found",
                "No identical files were confirmed in this search. Turn Duplicates off to return to all results.",
            )
        } else if self.content {
            (
                IconName::SearchX,
                "No content matches",
                "Enter text to find inside files. Results depend on Spotlight indexing and may take time to appear.",
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
            .on_action(cx.listener(|this, _: &NextResult, window, cx| this.move_selection(1, false, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousResult, window, cx| this.move_selection(-1, false, window, cx)))
            .on_action(cx.listener(|_, _: &SearchHelp, window, cx| crate::settings_view::show_help(
                "Search filenames as you type.\n\nreport budget — both words\n\"annual report\" — exact phrase\n*.pdf | *.docx — either pattern\n!backup — exclude a word\n<cat | dog> photo — group terms\next:jpg;png — extension filter\nfile: / folder: — kind\npath:Projects — search full path\nsize:>10mb — size filter\nmodified:today — changed today\nmodified:thisweek — changed since Monday\nmodified:>=2026-10-01 — changed on or after October 1, 2026\nmodified:2026-01-01..2026-01-31 — inclusive dates\n\nFilename search automatically allows one typo in unquoted filename terms of 4–64 characters when Aa is off. Aa requires exact letters and case. Quotes, wildcards and predicates stay exact.\nSaved: name and restore the query, modes, filter, folder scope and sorting. Right-click a folder → Search here; clear the scope chip to search everywhere. Drag selected files out of the window into another app.\n\nAa: case sensitive. Path: match full paths. Regex: treat the entire query as a regular expression.\n\nContent: search text inside files using Spotlight. Type text or * / ? wildcards; the whole query is a content pattern. Aa controls case. Path, Regex and filename operators apply only to filename search. Results are limited to indexed files; Spotlight coverage and updates may lag.\n\n⌘-click Toggle selection · Shift-click Select range · ⌘A Select all · ⇧↑/↓ Extend selection · ⌘C Copy files · ⌘⌫ Move to Trash\n\n⌘F Search · ↓/↑ Select · Space Quick Look · Return Open · ⌘Return Reveal · ⇧⌘C Copy path · ⌘R Rebuild · ⌘, Folders".into(), window, cx)))
            .child(div().flex().items_center().gap_2().px_5().pt_4().pb_3()
                .child(div().flex_1().min_w_0().child(Input::new(&self.input).id("search-input").aria_label(if self.content { "Search file contents" } else { "Search files and folders" })
                .prefix(self.search_history(window, cx))
                .suffix(div().text_xs().text_color(cx.theme().muted_foreground).child("⌘ F"))
                .cleanable(true)))
                .child(self.saved_searches(cx))
                .child(Button::new("help").ghost().icon(IconName::CircleQuestionMark).accessibility_label("Search help").tooltip("Search syntax and keyboard shortcuts").on_click(|_, window, cx| window.dispatch_action(Box::new(SearchHelp), cx))))
            .child(div().flex().flex_wrap().items_center().justify_between().px_5().pb_3().gap_2()
                .child(div().flex().items_center().gap_1().children([
                    ("Everything", IconName::Layers), ("Files", IconName::File), ("Folders", IconName::Folder),
                    ("Audio", IconName::Music2), ("Images", IconName::Image), ("Videos", IconName::Video), ("Documents", IconName::FileText)
                ].into_iter().enumerate().map(|(filter, (label, icon))| {
                    Button::new(("filter", filter)).ghost().small().icon(icon).label(label).selected(self.filter == filter).toggled(self.filter == filter)
                        .when(self.filter == filter, |this| this.bg(cx.theme().selection).text_color(cx.theme().link))
                        .on_click(cx.listener(move |this, _, window, cx| this.change_filter(filter, window, cx)))
                })))
                .child(div().id("search-options").test_support().role(Role::Group).aria_label("Search options")
                    .flex().flex_shrink_0().items_center().gap_1().p_1().rounded_lg().border_1().border_color(cx.theme().border).bg(cx.theme().background.opacity(0.55)).text_color(cx.theme().muted_foreground)
                    .child(Button::new("content").ghost().small().rounded_md().icon(if self.content { IconName::Check } else { IconName::FileText }).accessibility_label("Search file contents").tooltip("Search inside files content").selected(self.content).toggled(self.content).when(self.content, |this| this.bg(cx.theme().selection).text_color(cx.theme().link)).on_click(cx.listener(|this, _, window, cx| { this.content = !this.content; this.search(window, cx, true); })))
                    .child(div().w(px(1.)).h_4().mx_1().bg(cx.theme().border))
                    .child(Button::new("case").ghost().small().rounded_md().icon(IconName::CaseSensitive).accessibility_label("Case sensitive").tooltip("Match uppercase and lowercase exactly").selected(self.case).toggled(self.case).when(self.case, |this| this.bg(cx.theme().selection).text_color(cx.theme().link)).on_click(cx.listener(|this, _, window, cx| { this.case = !this.case; this.search(window, cx, true); })))
                    .child(div().flex().items_center().gap_1().when(self.content, |this| this.opacity(0.45))
                        .child(Button::new("path").ghost().small().rounded_md().icon(IconName::FolderTree).accessibility_label("Search full paths").tooltip(if self.content { "Path is unavailable in Content mode" } else { "Match the full folder path as well as the filename" }).disabled(self.content).selected(self.path).toggled(self.path).when(self.path && !self.content, |this| this.bg(cx.theme().selection).text_color(cx.theme().link)).on_click(cx.listener(|this, _, window, cx| { this.path = !this.path; this.search(window, cx, true); })))
                        .child(Button::new("regex").ghost().small().rounded_md().icon(IconName::Regex).accessibility_label("Use regular expressions").tooltip(if self.content { "Regex is unavailable in Content mode" } else { "Treat the query as a regular expression" }).disabled(self.content).selected(self.regex).toggled(self.regex).when(self.regex && !self.content, |this| this.bg(cx.theme().selection).text_color(cx.theme().link)).on_click(cx.listener(|this, _, window, cx| { this.regex = !this.regex; this.search(window, cx, true); }))))
                    ))
            .children(self.scope.clone().map(|scope| div().id("search-scope").test_support().role(Role::Status).aria_label(format!("Searching within {}", scope.display())).mx_5().mb_3().flex().items_center().gap_2().text_sm()
                .child(Icon::new(IconName::Folder).small())
                .child(div().flex_1().min_w_0().truncate().child(format!("Within: {}", scope.display())))
                .child(Button::new("clear-scope").ghost().small().icon(IconName::X).accessibility_label("Clear folder scope").tooltip("Search all indexed folders").on_click(cx.listener(|this, _, window, cx| this.set_scope(None, window, cx))))))
            .when(self.content, |this| this.child(div().id("content-search-note").test_support().role(Role::Status).aria_label("Content search uses Spotlight").mx_5().mb_3().text_xs().text_color(cx.theme().muted_foreground).child("Search inside files: text or * / ? wildcards. Spotlight may omit unindexed files or recent edits.")))
            .when(self.finding_duplicates, |this| this.child(div().id("duplicate-progress").test_support().role(Role::Status).aria_label("Comparing matching files for duplicates").mx_5().mb_3().flex().items_center().gap_2().text_sm()
                .child(Spinner::new().small()).child(div().flex_1().child("Comparing matching files for duplicates…"))
                .child(Button::new("cancel-duplicates").ghost().small().label("Cancel").on_click(cx.listener(|this, _, window, cx| this.search(window, cx, true))))))
            .children(self.duplicates.clone().map(|report| div().id("duplicate-summary").test_support().role(Role::Status).aria_label(format!("{} duplicate groups, {} files, {} skipped", report.group_ends.len(), report.results.len(), report.skipped)).mx_5().mb_3().flex().flex_col().gap_1().text_xs().text_color(cx.theme().muted_foreground)
                .child(format!("{} duplicate groups · {} files · {} skipped. Scan snapshot; turn Duplicates off to search again. Hard links count once.", format_count(report.group_ends.len()), format_count(report.results.len()), format_count(report.skipped)))
                .children(report.warning.clone())
                .child(Button::new("close-duplicates").ghost().small().label("Back to search").on_click(cx.listener(|this, _, window, cx| this.search(window, cx, true))))))
            .children(self.error.clone().map(|error| div().id("search-error").test_support().role(Role::Status).aria_label(error.clone())
                .mx_5().mb_3().px_3().py_2().rounded_md().bg(cx.theme().danger.opacity(0.08)).flex().items_center().gap_2().text_sm().text_color(cx.theme().danger)
                .child(Icon::new(IconName::CircleAlert).small().flex_shrink_0()).child(div().flex_1().min_w_0().child(error))))
            .child(div().flex().items_center().px_3().border_y_1().border_color(cx.theme().border).bg(cx.theme().muted).text_color(cx.theme().muted_foreground)
                .child(self.heading("Name", Sort::Name, 280., cx))
                .child(div().flex_1().min_w_0().child(self.heading("Path", Sort::Path, 140., cx)))
                .child(self.heading("Size", Sort::Size, 90., cx))
                .child(div().w(px(90.)).text_sm().py_1().child("Type"))
                .child(self.heading("Modified", Sort::Modified, 110., cx)))
            .child(div().id("results-table").test_support().track_focus(&self.results_focus).key_context("FileResults")
                .on_action(cx.listener(|this, _: &SelectAllResults, _, cx| this.select_all(cx)))
                .on_action(cx.listener(|this, _: &CopyFiles, _, cx| this.copy_files(cx)))
                .on_action(cx.listener(|this, _: &TrashResults, window, cx| this.trash(window, cx)))
                .on_action(cx.listener(|this, _: &ExtendNextResult, window, cx| this.move_selection(1, true, window, cx)))
                .on_action(cx.listener(|this, _: &ExtendPreviousResult, window, cx| this.move_selection(-1, true, window, cx)))
                .on_action(cx.listener(|this, _: &QuickLook, window, cx| this.quick_look(window, cx)))
                .role(Role::Table).aria_label(format!("{} results", self.results.len())).aria_row_count(self.results.len()).relative().flex_1().min_h_0().bg(cx.theme().table).child(
                uniform_list("results", results.len(), move |range, _, cx| {
                    range.map(|i| {
                        let entry = &results[i];
                        let name = entry.path.file_name().unwrap_or(entry.path.as_os_str()).to_string_lossy().into_owned();
                        let parent = entry.path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                        let kind = if entry.is_dir { "Folder".into() } else { entry.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_else(|| "File".into()) };
                        let group = duplicates.as_ref().map(|report| report.group_ends.partition_point(|end| *end <= i) + 1);
                        let owner = owner.clone();
                        let click_owner = owner.clone();
                        let mouse_owner = owner.clone();
                        let drag_owner = owner.clone();
                        let action_context = action_context.clone();
                        let menu_path = entry.path.clone();
                        let is_directory = entry.is_dir;
                        div().id(("result", i)).test_support().role(Role::Row).aria_label(group.map_or_else(|| entry.path.to_string_lossy().into_owned(), |group| format!("Duplicate group {group}: {}", entry.path.display())))
                            .aria_selected(selected_paths.contains(&entry.path)).flex().items_center().h(RESULT_ROW_HEIGHT).px_3().text_sm().w_full().cursor_pointer()
                            .when(i % 2 == 1, |row| row.bg(cx.theme().table_even))
                            .hover(|row| row.bg(if selected_paths.contains(&entry.path) { cx.theme().selection } else { cx.theme().table_hover }))
                            .when(selected_paths.contains(&entry.path), |row| row.bg(cx.theme().selection))
                            .child(div().w(px(280.)).flex_shrink_0().pr_3().flex().items_center().gap_2()
                                .child(Icon::new(file_icon(entry)).small().flex_shrink_0()
                                    .text_color(if entry.is_dir { cx.theme().link } else { cx.theme().muted_foreground }))
                                .child(div().min_w_0().truncate().child(group.map_or_else(|| name.clone(), |group| format!("#{group}  {name}")))))
                            .child(div().id(("result-path", i)).test_support().flex_1().min_w_0().pr_3().truncate().text_color(cx.theme().muted_foreground).child(parent.clone())
                                .tooltip(move |window, cx| {
                                    let parent = parent.clone();
                                    let width = (window.viewport_size().width - px(64.)).min(px(560.));
                                    Tooltip::element(move |_, _| div().id("path-tooltip-content").test_support().aria_label(parent.clone()).w(width).whitespace_normal().child(parent.clone()))
                                        .build(window, cx)
                                }))
                            .child(div().w(px(90.)).flex_shrink_0().child(if entry.is_dir { "—".into() } else { format_size(entry.allocated_size) }))
                            .child(div().w(px(90.)).flex_shrink_0().truncate().child(kind))
                            .child(div().w(px(110.)).flex_shrink_0().child(format_modified(entry.modified)))
                            .capture_any_mouse_down(move |event, window, cx| {
                                let _ = mouse_owner.update(cx, |this, cx| {
                                    let preserve = !event.modifiers.shift && !event.modifiers.platform && this.selected_paths.len() > 1 && this.results.get(i).is_some_and(|entry| this.selected_paths.contains(&entry.path));
                                    this.select_with_modifiers(i, event.modifiers, event.button == MouseButton::Right || event.modifiers.control || preserve, window, cx);
                                });
                            })
                            .on_click(move |event, window, cx| {
                                let _ = click_owner.update(cx, |this, cx| {
                                    if !matches!(event, ClickEvent::Mouse(_)) || matches!(event, ClickEvent::Mouse(event) if !event.down.modifiers.shift && !event.down.modifiers.platform && !event.down.modifiers.control) { this.select_result(i, window, cx); }
                                    if event.click_count() == 2 { this.open(false, window, cx); }
                                });
                            })
                            .on_drag(entry.path.clone(), |_, _, _, cx| cx.new(|_| Empty))
                            .external_drag_payload(move |path: &PathBuf, _, cx| {
                                drag_owner.update(cx, |this, _| ExternalDragPayload::Files(this.drag_paths(path))).ok()
                            })
                            .context_menu(move |menu, window, cx| {
                                // Focus before prepaint so accessibility sees only the menu as focused this frame.
                                menu.focus_handle(cx).focus(window, cx);
                                let dismiss_owner = owner.clone();
                                // The menu’s temporary trigger loses its focus path when dismissed.
                                cx.subscribe_in(&cx.entity(), window, move |_, _, _: &DismissEvent, window, cx| {
                                    let _ = dismiss_owner.update(cx, |this, cx| {
                                        if !this.input.focus_handle(cx).is_focused(window) && !window.has_active_dialog(cx) {
                                            this.results_focus.focus(window, cx);
                                        }
                                    });
                                }).detach();
                                let multiple = owner.upgrade().is_some_and(|owner| owner.read(cx).selected_paths.len() > 1);
                                let info_owner = owner.clone();
                                let info_path = menu_path.clone();

                                let rename_path = menu_path.clone();
                                let scope_path = menu_path.clone();
                                let scope_owner = owner.clone();
                                menu.action_context(action_context.clone())
                                    .menu_with_icon("Open", IconName::ExternalLink, Box::new(OpenResult))
                                    .menu_with_icon("Show in Finder", IconName::FolderOpen, Box::new(RevealResult))
                                    .item(PopupMenuItem::new("Get Info").icon(IconName::Info).disabled(multiple)
                                        .on_click(move |_, window, cx| {
                                            let _ = info_owner.update(cx, |this, cx| this.run_command(
                                                crate::file_actions::get_info_command(&info_path), "get file info", window, cx));
                                        }))
                                    .separator()
                                    .menu_with_icon("Copy", IconName::Copy, Box::new(CopyFiles))
                                    .menu_with_icon(if multiple { "Copy paths" } else { "Copy path" }, IconName::Copy, Box::new(CopyPath))
                                    .item(PopupMenuItem::new("Rename…").icon(IconName::Pencil).disabled(multiple)
                                        .on_click(move |_, window, cx| {
                                            let path = rename_path.clone();
                                            window.defer(cx, move |window, cx| crate::file_actions::show_rename(path, window, cx));
                                        }))
                                    .separator()
                                    .menu_with_icon("Move to Trash", IconName::Trash, Box::new(TrashResults))
                                    .item(PopupMenuItem::new("Search here").icon(IconName::Search).disabled(!is_directory || multiple)
                                        .on_click(move |_, window, cx| {
                                            let scope_path = scope_path.clone();
                                            let scope_owner = scope_owner.clone();
                                            window.defer(cx, move |window, cx| { let _ = scope_owner.update(cx, |this, cx| this.set_scope(Some(scope_path), window, cx)); });
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
                            this.content = false;
                            this.sort = Sort::BestMatch;
                            this.descending = false;
                            this.scope = None;
                            this.input.update(cx, |input, cx| { input.set_value("", window, cx); input.focus(window, cx); });
                            this.search(window, cx, true);
                        })))))))
            .child(div().flex().items_center().gap_2().px_5().py_3().border_t_1().border_color(cx.theme().border).bg(cx.theme().status_bar)
                .child(div().flex().flex_1().items_center().gap_2()
                .child(Button::new("open").primary().small().icon(IconName::ExternalLink).label("Open").tooltip("Open selected results · Return").disabled(self.selected.is_none()).on_click(cx.listener(|this, _, window, cx| this.open(false, window, cx))))
                .child(Button::new("reveal").ghost().small().icon(IconName::FolderOpen).label("Show in Finder").tooltip("Reveal selected results · ⌘Return").disabled(self.selected.is_none()).on_click(cx.listener(|this, _, window, cx| this.open(true, window, cx))))
                .child(Button::new("copy-path").ghost().small().icon(IconName::Copy).label(if self.selected_paths.len() > 1 { "Copy paths" } else { "Copy path" }).tooltip("Copy selected paths · ⇧⌘C").disabled(self.selected.is_none()).on_click(cx.listener(|this, _, _, cx| this.copy(cx)))))
                .child(div().id("result-count").test_support().role(Role::Status).aria_label(if self.selected_paths.len() > 1 { format!("{} matches, {} selected", self.results.len(), self.selected_paths.len()) } else { format!("{} matches", self.results.len()) })
                    .flex_shrink_0().text_xs().text_color(cx.theme().muted_foreground).child(if self.selected_paths.len() > 1 { format!("{} matches · {} selected", format_count(self.results.len()), format_count(self.selected_paths.len())) } else { format!("{} matches", format_count(self.results.len())) }))
                .child(div().flex().flex_1().items_center().justify_end().gap_2()
                .child(Button::new("full-disk-access").ghost().small().icon(IconName::TriangleAlert).accessibility_label("Needs Full Disk Access")
                    .tooltip(self.snapshot.scan_warning.clone().unwrap_or_else(|| "Open Full Disk Access in System Settings".into()))
                    .on_click(|_, _, cx| cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")))
                .child(div().id("navigation-hints").test_support().text_xs().text_color(cx.theme().muted_foreground).child("↑ ↓ Navigate    Space Quick Look    ↵ Open    ⌘ ↵ Reveal"))))
            .children(self.snapshot.warning.clone().map(|warning| div().id("index-warnings").test_support().role(Role::Status).aria_label(warning.clone()).max_h(px(80.)).overflow_y_scroll().px_5().pb_2().bg(cx.theme().status_bar).text_xs().text_color(cx.theme().muted_foreground).child(warning)))
            .into_any_element()
    }
}

fn file_icon(entry: &Entry) -> IconName {
    if entry.is_dir {
        return IconName::Folder;
    }
    let extension = entry
        .path
        .extension()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.to_str().unwrap_or_default() {
        "mp3" | "m4a" | "aac" | "flac" | "wav" | "aiff" | "ogg" | "opus" => IconName::Music2,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "tif" | "tiff" | "bmp"
        | "svg" | "raw" | "ico" | "avif" => IconName::Image,
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" | "mpeg" | "mpg" => IconName::Video,
        "pdf" | "doc" | "docx" | "txt" | "md" | "rtf" | "pages" | "odt" | "log" => {
            IconName::FileText
        }
        "xls" | "xlsx" | "csv" | "tsv" | "numbers" | "ods" => IconName::FileSpreadsheet,
        "ppt" | "pptx" | "key" | "odp" => IconName::Presentation,
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" | "tgz" | "zst" => IconName::FileArchive,
        "rs" | "swift" | "c" | "h" | "cpp" | "hpp" | "m" | "mm" | "py" | "js" | "jsx" | "ts"
        | "tsx" | "html" | "css" | "scss" | "json" | "xml" | "yaml" | "yml" | "toml" | "java"
        | "kt" | "go" | "rb" | "php" | "sql" => IconName::FileCode,
        "sh" | "bash" | "zsh" | "fish" | "command" | "bat" | "ps1" => IconName::Terminal,
        "db" | "sqlite" | "sqlite3" => IconName::Database,
        "dmg" | "iso" | "img" => IconName::HardDrive,
        "ttf" | "otf" | "woff" | "woff2" => IconName::FileType,
        _ => IconName::File,
    }
}

fn compare_entries(a: &Entry, b: &Entry, sort: Sort, query: Option<&Query>) -> std::cmp::Ordering {
    let order = match sort {
        Sort::BestMatch => query.map_or(std::cmp::Ordering::Equal, |query| {
            query
                .rank(&a.path, a.is_dir, a.size, a.modified)
                .cmp(&query.rank(&b.path, b.is_dir, b.size, b.modified))
        }),
        Sort::Name => a
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
            .cmp(
                &b.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase(),
            ),
        Sort::Size => a.allocated_size.cmp(&b.allocated_size),
        Sort::Modified => a.modified.cmp(&b.modified),
        Sort::Path => std::cmp::Ordering::Equal,
    };
    order.then_with(|| a.path.cmp(&b.path))
}

fn merge_results(
    previous: &[Arc<Entry>],
    current: &[Arc<Entry>],
    sort: Sort,
    descending: bool,
    query: Option<&Query>,
) -> Vec<Arc<Entry>> {
    let mut matching: HashMap<_, _> = current
        .iter()
        .map(|entry| (entry.path.as_path(), entry))
        .collect();
    let retained: Vec<_> = previous
        .iter()
        .filter_map(|old| {
            let new = *matching.get(old.path.as_path())?;
            (Arc::ptr_eq(old, new) || old == new)
                .then(|| matching.remove(old.path.as_path()).unwrap().clone())
        })
        .collect();
    let mut changed: Vec<_> = current
        .iter()
        .filter(|entry| matching.contains_key(entry.path.as_path()))
        .cloned()
        .collect();
    let compare = |a: &Arc<Entry>, b: &Arc<Entry>| {
        let order = compare_entries(a, b, sort, query);
        if descending { order.reverse() } else { order }
    };
    changed.sort_by(compare);
    let mut retained = retained.into_iter().peekable();
    let mut changed = changed.into_iter().peekable();
    let mut merged = Vec::with_capacity(current.len());
    while let (Some(old), Some(new)) = (retained.peek(), changed.peek()) {
        if compare(old, new).is_le() {
            merged.push(retained.next().unwrap());
        } else {
            merged.push(changed.next().unwrap());
        }
    }
    merged.extend(retained);
    merged.extend(changed);
    merged
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

fn trash_paths(mut paths: Vec<PathBuf>) -> Result<(), String> {
    paths.sort();
    paths.dedup();
    let mut moved_folders: Vec<PathBuf> = Vec::new();
    let mut failures = Vec::new();
    for path in paths {
        if moved_folders.iter().any(|parent| path.starts_with(parent)) {
            continue;
        }
        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir());
        match move_to_trash(&path) {
            Ok(_) => {
                if is_dir {
                    moved_folders.push(path);
                }
            }
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
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
    use super::{SearchView, file_icon, format_count, format_size, kind_matches, move_to_trash};
    use crate::{index::Entry, settings::Settings};
    use gpui_kit::assets::IconName;
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use gpui_kit::{AppContext, ScrollStrategy, TestAppContext};
    use std::{path::PathBuf, sync::Arc};

    #[gpui_kit::test]
    async fn default_search_ranks_matches_and_column_sort_still_works(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
        });
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1150.), gpui_kit::px(450.)),
            |window, cx| {
                window.set_scroll_layers(false);
                SearchView::new(None, window, cx)
            },
        );
        window
            .update(cx, |view, window, cx| {
                view._poll = gpui_kit::Task::ready(());
                view.search_task = None;
                view.snapshot.loaded = true;
                view.snapshot.entries = Arc::new(
                    [
                        "/a/anual.pdf",
                        "/b/annual-report.pdf",
                        "/c/annual",
                        "/d/old-annual.pdf",
                        "/e/annual.txt",
                    ]
                    .map(|path| {
                        Arc::new(Entry {
                            path: path.into(),
                            is_dir: false,
                            size: 0,
                            allocated_size: 0,
                            modified: None,
                        })
                    })
                    .into(),
                );
                view.input
                    .update(cx, |input, cx| input.set_value("annual", window, cx));
                view.search(window, cx, true);
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("best-match").is_none());
            assert!(window.try_find("fuzzy").is_none());
        })
        .unwrap();
        window
            .update(cx, |view, _, _| view.search_task.take().unwrap())
            .unwrap()
            .await;
        window
            .update(cx, |view, _, _| {
                assert!(!view.searching && !view.sorting);
                assert_eq!(
                    view.results
                        .iter()
                        .map(|entry| entry.path.to_str().unwrap())
                        .collect::<Vec<_>>(),
                    [
                        "/c/annual",
                        "/b/annual-report.pdf",
                        "/e/annual.txt",
                        "/d/old-annual.pdf",
                        "/a/anual.pdf",
                    ]
                );
                assert_eq!(
                    view.filter_results[0].as_ref().unwrap().sort,
                    crate::saved_searches::Sort::BestMatch
                );
            })
            .unwrap();
        for control in ["Name", "Path"] {
            cx.update_window(window.into(), |_, window, cx| {
                window.render_frame(cx);
                window.click(control, cx);
            })
            .unwrap();
            window
                .update(cx, |view, _, _| view.search_task.take().unwrap())
                .unwrap()
                .await;
        }
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.sort, crate::saved_searches::Sort::Path);
                assert_eq!(view.results[0].path, PathBuf::from("/a/anual.pdf"));
            })
            .unwrap();
        let task = window
            .update(cx, |view, window, cx| {
                view.change_filter(1, window, cx);
                view.search_task.take().unwrap()
            })
            .unwrap();
        task.await;
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.sort, crate::saved_searches::Sort::BestMatch);
                assert_eq!(view.results[0].path, PathBuf::from("/c/annual"));
            })
            .unwrap();
    }

    #[gpui_kit::test]
    async fn deleting_input_does_not_compile_queries_inline_or_publish_stale_errors(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
        });
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1050.), gpui_kit::px(450.)),
            |window, cx| SearchView::new(None, window, cx),
        );
        window
            .update(cx, |view, _, _| {
                view._poll = gpui_kit::Task::ready(());
                view.search_task = None;
                view.regex = true;
                view.snapshot.entries = Arc::new(vec![Arc::new(Entry {
                    path: "/fixture/file.txt".into(),
                    is_dir: false,
                    size: 0,
                    allocated_size: 0,
                    modified: None,
                })]);
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.click("search-input", cx);
            window.input("[", cx);
        })
        .unwrap();
        let invalid = window
            .update(cx, |view, _, _| {
                assert!(view.error.is_none(), "Parsing must run on the worker");
                view.search_task.take().unwrap()
            })
            .unwrap();
        invalid.await;
        window
            .update(cx, |view, _, _| assert!(view.error.is_some()))
            .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.press("backspace", cx);
            assert_eq!(window.find("search-input").value(), Some(""));
            window.input("[é]", cx);
            for expected in ["[é", "[", ""] {
                window.press("backspace", cx);
                assert_eq!(window.find("search-input").value(), Some(expected));
            }
        })
        .unwrap();
        let latest = window
            .update(cx, |view, _, _| view.search_task.take().unwrap())
            .unwrap();
        latest.await;
        window
            .update(cx, |view, _, _| {
                assert!(view.error.is_none());
                assert!(!view.searching);
                assert_eq!(view.results.len(), 1);
            })
            .unwrap();
    }

    #[gpui_kit::test]
    fn applying_results_preserves_active_selection_and_handles_removed_paths(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
        });
        let window = cx.add_window(|window, cx| SearchView::new(None, window, cx));
        window
            .update(cx, |view, _, cx| {
                view._poll = gpui_kit::Task::ready(());
                view.search_task = None;
                let entries: Vec<_> = ["a", "b", "c"]
                    .map(|name| {
                        Arc::new(Entry {
                            path: PathBuf::from(name),
                            is_dir: false,
                            size: 0,
                            allocated_size: 0,
                            modified: None,
                        })
                    })
                    .into();
                view.results = Arc::new(entries.clone());
                view.selected = Some(2);
                view.selected_paths = Arc::new([PathBuf::from("a"), PathBuf::from("c")].into());
                view.selection_anchor = Some(PathBuf::from("c"));
                for (next, expected, paths) in [
                    (vec![2, 1, 0], Some(0), vec!["a", "c"]),
                    (vec![0, 1], Some(0), vec!["a"]),
                    (vec![1], Some(0), vec!["b"]),
                    (vec![], None, vec![]),
                    (vec![1, 2], Some(0), vec!["b"]),
                ] {
                    let results = next.into_iter().map(|i| entries[i].clone()).collect();
                    view.set_results(Arc::new(results), cx, false);
                    assert_eq!(view.selected, expected);
                    assert_eq!(
                        *view.selected_paths,
                        paths.into_iter().map(PathBuf::from).collect()
                    );
                    assert_eq!(
                        view.selection_anchor,
                        expected.map(|i| view.results[i].path.clone())
                    );
                }
            })
            .unwrap();
    }

    #[gpui_kit::test]
    fn drag_payload_contains_selected_paths_and_directory_metadata(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            })
        });
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1050.), gpui_kit::px(450.)),
            |window, cx| SearchView::new(None, window, cx),
        );
        window
            .update(cx, |view, _, _| {
                view.results = Arc::new(
                    [
                        ("/scope", true),
                        ("/scope/a.txt", false),
                        ("/other.txt", false),
                    ]
                    .into_iter()
                    .map(|(path, is_dir)| {
                        Arc::new(Entry {
                            path: path.into(),
                            is_dir,
                            size: 0,
                            allocated_size: 0,
                            modified: None,
                        })
                    })
                    .collect(),
                );
                view.selected_paths = Arc::new(
                    [PathBuf::from("/scope"), PathBuf::from("/other.txt")]
                        .into_iter()
                        .collect(),
                );
                assert_eq!(
                    view.drag_paths(std::path::Path::new("/scope")).entries(),
                    &[
                        (PathBuf::from("/scope"), true),
                        (PathBuf::from("/other.txt"), false)
                    ]
                );
                assert_eq!(
                    view.drag_paths(std::path::Path::new("/scope/a.txt"))
                        .entries(),
                    &[(PathBuf::from("/scope/a.txt"), false)]
                );
                assert!(
                    view.drag_paths(std::path::Path::new("/missing"))
                        .entries()
                        .is_empty()
                );
            })
            .unwrap();
    }

    #[gpui_kit::test]
    async fn startup_hides_partial_results_until_the_complete_search_finishes(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.set_global(Settings {
                roots: vec![PathBuf::from("/fixture")],
                excluded: Vec::new(),
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
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
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
                view.snapshot.scanning = false;
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
    async fn incremental_updates_match_explicit_sort_for_every_column(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
        });
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1050.), gpui_kit::px(450.)),
            |window, cx| SearchView::new(None, window, cx),
        );
        let entry = |path: &str, size, modified: Option<u64>| {
            Arc::new(Entry {
                path: path.into(),
                is_dir: false,
                size,
                allocated_size: size * 4096,
                modified: modified.map(|seconds| {
                    std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds)
                }),
            })
        };
        let previous = Arc::new(vec![
            entry("/a/Report.txt", 2, Some(10)),
            entry("/b/report.txt", 4, Some(20)),
            entry("/c/Report.txt", 2, None),
            entry("/d/report-old.txt", 8, Some(10)),
            entry("/e/REPORT.pdf", 0, Some(30)),
            entry("/f/report", 4, Some(20)),
        ]);
        let current = Arc::new(vec![
            previous[0].clone(),
            entry("/c/Report.txt", 9, Some(40)),
            previous[3].clone(),
            previous[4].clone(),
            previous[5].clone(),
            entry("/g/REPORT.txt", 2, Some(10)),
            entry("/h/report", 4, None),
            entry("/i/unrelated.bin", 0, None),
        ]);
        window
            .update(cx, |view, window, cx| {
                view._poll = gpui_kit::Task::ready(());
                view.input
                    .update(cx, |input, cx| input.set_value("report", window, cx));
            })
            .unwrap();
        for sort in [
            crate::saved_searches::Sort::BestMatch,
            crate::saved_searches::Sort::Name,
            crate::saved_searches::Sort::Size,
            crate::saved_searches::Sort::Modified,
            crate::saved_searches::Sort::Path,
        ] {
            for descending in [false, true] {
                window
                    .update(cx, |view, window, cx| {
                        view.snapshot.entries = previous.clone();
                        view.snapshot.loaded = true;
                        view.snapshot.scanning = false;
                        view.sort = sort;
                        view.descending = descending;
                        view.search(window, cx, false);
                        view.search_task.take().unwrap()
                    })
                    .unwrap()
                    .await;
                window
                    .update(cx, |view, window, cx| {
                        let mut snapshot = view.snapshot.clone();
                        snapshot.revision += 1;
                        snapshot.entries = current.clone();
                        view.update_snapshot(snapshot, window, cx);
                        view.search_task.take().unwrap()
                    })
                    .unwrap()
                    .await;
                let merged = window
                    .update(cx, |view, _, _| view.results.clone())
                    .unwrap();
                window
                    .update(cx, |view, window, cx| {
                        view.search(window, cx, false);
                        view.search_task.take().unwrap()
                    })
                    .unwrap()
                    .await;
                window
                    .update(cx, |view, _, _| {
                        assert_eq!(merged, view.results, "{sort:?}, descending={descending}");
                        assert_eq!(
                            merged.len(),
                            7,
                            "The active query must exclude unrelated files"
                        );
                    })
                    .unwrap();
            }
        }
    }

    #[gpui_kit::test]
    async fn live_updates_merge_changed_files_without_sorting_existing_rows(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
        });
        let window = cx.open_window(
            gpui_kit::size(gpui_kit::px(1050.), gpui_kit::px(450.)),
            |window, cx| SearchView::new(None, window, cx),
        );
        let entries: Vec<_> = [2, 3, 1, 4, 0]
            .into_iter()
            .enumerate()
            .map(|(i, size)| {
                Arc::new(Entry {
                    path: PathBuf::from(format!("/fixture/file-{i}.txt")),
                    is_dir: false,
                    size,
                    allocated_size: size * 4096,
                    modified: None,
                })
            })
            .collect();
        let task = window
            .update(cx, |view, window, cx| {
                view._poll = gpui_kit::Task::ready(());
                view.search_task = None;
                view.snapshot.loaded = true;
                view.snapshot.scanning = false;
                view.snapshot.entries = Arc::new(entries[..3].to_vec());
                view.sort = crate::saved_searches::Sort::Size;
                view.descending = true;
                view.search(window, cx, false);
                view.search_task.take().unwrap()
            })
            .unwrap();
        task.await;
        let task = window
            .update(cx, |view, window, cx| {
                assert_eq!(
                    view.results
                        .iter()
                        .map(|entry| entry.size)
                        .collect::<Vec<_>>(),
                    [3, 2, 1]
                );
                let displayed = view.results.clone();
                let generation = view.generation.load(std::sync::atomic::Ordering::Relaxed);
                for _ in 0..10 {
                    let mut snapshot = view.snapshot.clone();
                    snapshot.scanning = true;
                    snapshot.revision += 1;
                    snapshot.entries = Arc::new(entries[..4].to_vec());
                    view.update_snapshot(snapshot, window, cx);
                    assert!(view.search_task.is_none());
                    assert!(Arc::ptr_eq(&view.results, &displayed));
                    assert_eq!(
                        view.generation.load(std::sync::atomic::Ordering::Relaxed),
                        generation
                    );
                }
                // Explicit sorting still works while scanning, even with newer snapshots queued.
                view.descending = false;
                view.search(window, cx, false);
                let mut snapshot = view.snapshot.clone();
                snapshot.revision += 1;
                snapshot.entries = Arc::new(entries);
                view.update_snapshot(snapshot, window, cx);
                view.search_task.take().unwrap()
            })
            .unwrap();
        task.await;
        let view = window
            .update(cx, |view, _, cx| {
                assert!(
                    view.search_task.is_none(),
                    "Wait for scan completion to refresh"
                );
                assert_eq!(
                    view.results
                        .iter()
                        .map(|entry| entry.size)
                        .collect::<Vec<_>>(),
                    [1, 2, 3, 4]
                );
                cx.entity()
            })
            .unwrap();
        let displayed = cx.update(|cx| view.read(cx).results.clone());
        let sorting_updates = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = sorting_updates.clone();
        let _subscription = cx.update(|cx| {
            cx.observe(&view, move |view, cx| {
                let view = view.read(cx);
                if view.sorting {
                    observed.set(observed.get() + 1);
                    assert!(
                        Arc::ptr_eq(&view.results, &displayed),
                        "Keep unsorted results hidden"
                    );
                }
            })
        });
        let task = window
            .update(cx, |view, window, cx| {
                let mut snapshot = view.snapshot.clone();
                snapshot.scanning = false;
                snapshot.revision += 1;
                view.update_snapshot(snapshot, window, cx);
                view.search_task.take().unwrap()
            })
            .unwrap();
        task.await;
        assert_eq!(sorting_updates.get(), 0, "Live updates must not sort");
        cx.update(|cx| {
            let view = view.read(cx);
            assert!(!view.searching && !view.sorting);
            assert!(view.search_task.is_none());
            assert_eq!(
                view.results
                    .iter()
                    .map(|entry| entry.size)
                    .collect::<Vec<_>>(),
                [0, 1, 2, 3, 4]
            );
        });
        let task = window
            .update(cx, |view, window, cx| {
                let mut snapshot = view.snapshot.clone();
                snapshot.revision += 1;
                let entries = Arc::make_mut(&mut snapshot.entries);
                entries.remove(0);
                let resized = Arc::make_mut(&mut entries[1]);
                resized.size = 7;
                resized.allocated_size = 7 * 4096;
                view.update_snapshot(snapshot, window, cx);
                view.search_task.take().unwrap()
            })
            .unwrap();
        task.await;
        cx.update(|cx| {
            let view = view.read(cx);
            assert_eq!(
                view.results
                    .iter()
                    .map(|entry| entry.size)
                    .collect::<Vec<_>>(),
                [0, 3, 4, 7],
                "Remove deleted files and reinsert resized files at their sorted position"
            );
        });
        assert_eq!(sorting_updates.get(), 0);
        drop(_subscription);
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("Size", cx);
        })
        .unwrap();
        window
            .update(cx, |view, _, _| view.search_task.take().unwrap())
            .unwrap()
            .await;
        cx.update(|cx| {
            let view = view.read(cx);
            assert!(!view.searching && !view.sorting);
            assert_eq!(
                view.results
                    .iter()
                    .map(|entry| entry.size)
                    .collect::<Vec<_>>(),
                [7, 4, 3, 0],
                "Clicking Size must sort all current files"
            );
        });
    }

    #[gpui_kit::test]
    fn first_scan_shows_progress_and_keeps_search_available(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
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
        let folder = directory.join(directory.file_name().unwrap());
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
        let destination = move_to_trash(&folder).unwrap().unwrap();
        fs::rename(&destination, &folder).unwrap();
        super::trash_paths(vec![folder.join("nested.txt"), folder.clone()]).unwrap();
        assert!(!folder.exists());
        fs::rename(destination, &folder).unwrap();
        assert_eq!(
            fs::read(folder.join("nested.txt")).unwrap(),
            b"nested content"
        );
        let failure = super::trash_paths(vec![
            directory.join("missing-one"),
            directory.join("missing-two"),
        ])
        .unwrap_err();
        assert!(failure.contains("missing-one") && failure.contains("missing-two"));
        assert!(move_to_trash(&directory.join("missing")).is_err());
        assert!(move_to_trash(&directory.join("invalid\0path")).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[gpui_kit::test]
    async fn rebuild_cache_is_in_settings_and_rescans(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (handle, view) = cx.update(|cx| {
            cx.set_reduce_motion(true);
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
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
        cx.update(|cx| {
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            })
        });
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
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
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
            window.input("\"file-049\"", cx);
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
                snapshot.scanning = false;
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
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
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
        let (offset, height) = window
            .update(cx, |view, _, _| {
                let state = view.scroll.0.borrow();
                (
                    state.base_handle.offset(),
                    state.last_item_size.unwrap().contents.height / view.results.len() as f32,
                )
            })
            .unwrap();
        let task = window
            .update(cx, |view, window, cx| {
                let mut snapshot = view.snapshot.clone();
                snapshot.scanning = false;
                snapshot.revision += 1;
                Arc::make_mut(&mut snapshot.entries).insert(
                    200,
                    Arc::new(Entry {
                        path: "/fixture/file-1995.txt".into(),
                        is_dir: false,
                        allocated_size: 200 * 4096 + 2048,
                        size: 300,
                        modified: None,
                    }),
                );
                view.update_snapshot(snapshot, window, cx);
                view.search_task.take().unwrap()
            })
            .unwrap();
        task.await;
        cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
        window
            .update(cx, |view, _, _| {
                assert_eq!(view.selected, Some(0));
                assert_eq!(
                    view.scroll.0.borrow().base_handle.offset().y,
                    offset.y - height
                );
            })
            .unwrap();
        cx.update_window(window.into(), |_, window, _| {
            assert_eq!(
                window.find(("result", 201usize)).label(),
                Some("/fixture/file-200.txt")
            );
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
            cx.set_global(Settings {
                roots: Vec::new(),
                excluded: Vec::new(),
            });
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
    fn file_icons_distinguish_types_and_preserve_folders_and_unknown_files() {
        for (name, expected) in [
            ("song.MP3", IconName::Music2),
            ("photo.JpG", IconName::Image),
            ("movie.mov", IconName::Video),
            ("report.pdf", IconName::FileText),
            ("notes.md", IconName::FileText),
            ("budget.xlsx", IconName::FileSpreadsheet),
            ("slides.key", IconName::Presentation),
            ("backup.tar.gz", IconName::FileArchive),
            ("main.rs", IconName::FileCode),
            ("view.tsx", IconName::FileCode),
            ("config.json", IconName::FileCode),
            ("run.command", IconName::Terminal),
            ("index.sqlite3", IconName::Database),
            ("installer.dmg", IconName::HardDrive),
            ("font.otf", IconName::FileType),
            ("unknown.xyz", IconName::File),
            ("README", IconName::File),
            (".gitignore", IconName::File),
            ("trailing.", IconName::File),
        ] {
            let mut entry = Entry {
                path: PathBuf::from("/tmp").join(name),
                is_dir: false,
                size: 0,
                allocated_size: 0,
                modified: None,
            };
            assert_eq!(file_icon(&entry), expected, "{name}");
            assert!(gpui_kit::assets::AllAssets::get(expected.path().as_ref()).is_some());
            entry.is_dir = true;
            assert_eq!(file_icon(&entry), IconName::Folder, "{name}");
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
