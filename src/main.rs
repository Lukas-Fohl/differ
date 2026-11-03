use bevy::prelude::*;
use bevy::winit::WinitSettings;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};
use differ::{
    cli,
    git::{self, Comparison, FileDiff, LineKind},
    view,
};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

struct Prepared {
    diff: FileDiff,
    layouts: [Vec<view::DisplayRow>; 4],
    width: usize,
}
impl Prepared {
    fn new(diff: FileDiff) -> Self {
        let layouts =
            std::array::from_fn(|i| view::display_rows(&diff.rows, i & 1 != 0, i & 2 != 0));
        let width = diff
            .rows
            .iter()
            .map(|r| r.text.replace('\t', "    ").chars().count())
            .max()
            .unwrap_or(0);
        Self {
            diff,
            layouts,
            width,
        }
    }
}

#[derive(Clone)]
struct History {
    branches: git::Branches,
    source: String,
    commits: Vec<git::CommitSummary>,
    has_more: bool,
}

type FileKey = (std::path::PathBuf, bool);

fn file_key(file: &git::ChangedFile) -> FileKey {
    (file.path.clone(), file.staged)
}
fn file_id(c: &Comparison, index: usize) -> egui::Id {
    egui::Id::new((&c.repo, file_key(&c.files[index])))
}
fn find_file(c: &Comparison, key: &FileKey) -> Option<usize> {
    c.files
        .iter()
        .position(|file| file_key(file) == *key)
        .or_else(|| c.files.iter().position(|file| file.path == key.0))
}
fn read_history(
    repo: &std::path::Path,
    source: Option<String>,
    limit: usize,
) -> Result<History, String> {
    let branches = git::branches(repo)?;
    let source = source.unwrap_or_else(|| {
        branches
            .current
            .as_ref()
            .filter(|name| branches.local.contains(name))
            .map(|name| format!("refs/heads/{name}"))
            .unwrap_or_else(|| "HEAD".into())
    });
    let mut commits = git::commit_history(repo, &source, 0, limit + 1)?;
    let has_more = commits.len() > limit;
    commits.truncate(limit);
    Ok(History {
        branches,
        source,
        commits,
        has_more,
    })
}

struct RefreshRequest {
    generation: u64,
    comparison: Arc<Comparison>,
    target: git::Target,
    selected: Option<FileKey>,
    source: Option<String>,
    limit: usize,
    refs: Option<Vec<u8>>,
}
struct RefreshResult {
    comparison: Arc<Comparison>,
    selected: Option<(usize, Arc<Prepared>)>,
    refs: Vec<u8>,
    history: Option<Result<History, String>>,
    branches_on_history_error: Option<git::Branches>,
}
fn refresh(request: &RefreshRequest) -> Result<RefreshResult, String> {
    let repo = &request.comparison.repo;
    let refs = git::refs_snapshot(repo)?;
    let history = (request.refs.as_ref() != Some(&refs))
        .then(|| read_history(repo, request.source.clone(), request.limit));
    let branches_on_history_error = if history.as_ref().is_some_and(Result::is_err) {
        Some(git::branches(repo)?)
    } else {
        None
    };
    let comparison = if matches!(request.target, git::Target::Commit(_)) {
        request.comparison.clone()
    } else {
        Arc::new(git::compare(repo, request.target.clone())?)
    };
    let selected = request
        .selected
        .as_ref()
        .and_then(|key| find_file(&comparison, key))
        .map(|index| {
            git::load_file(&comparison, index).map(|diff| (index, Arc::new(Prepared::new(diff))))
        })
        .transpose()?;
    Ok(RefreshResult {
        comparison,
        selected,
        refs,
        history,
        branches_on_history_error,
    })
}

enum Event {
    Refreshed(u64, Option<FileKey>, Result<RefreshResult, String>),
    Staging(u64, git::ChangedFile, Result<(), String>),
    Comparison(u64, git::Target, Result<Arc<Comparison>, String>),
    History(u64, Result<History, String>),
    File(u64, usize, Result<Arc<Prepared>, String>),
}

enum Request {
    Refresh(RefreshRequest),
    ToggleStaging(u64, std::path::PathBuf, Vec<git::ChangedFile>),
    File(u64, usize, Arc<Comparison>),
    Reload {
        generation: u64,
        repo: std::path::PathBuf,
        target: git::Target,
        source: Option<String>,
        limit: usize,
        select_tip: bool,
    },
}

#[derive(Default)]
struct Tree {
    directories: BTreeMap<OsString, Tree>,
    files: Vec<(OsString, usize)>,
}
impl Tree {
    fn new(c: &Comparison, staged: bool) -> Self {
        let mut root = Self::default();
        for (index, file) in c
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.staged == staged)
        {
            let parts: Vec<_> = file
                .path
                .components()
                .map(|c| c.as_os_str().to_owned())
                .collect();
            let mut node = &mut root;
            for part in &parts[..parts.len().saturating_sub(1)] {
                node = node.directories.entry(part.clone()).or_default();
            }
            if let Some(name) = parts.last() {
                node.files.push((name.clone(), index));
            }
        }
        root
    }
    fn file_indices(&self) -> Vec<usize> {
        let mut indices: Vec<_> = self.files.iter().map(|(_, index)| *index).collect();
        for tree in self.directories.values() {
            indices.extend(tree.file_indices());
        }
        indices
    }

    fn show(
        &self,
        ui: &mut egui::Ui,
        c: &Comparison,
        selected: &mut Option<usize>,
        action: &mut Option<Vec<usize>>,
        busy: bool,
    ) {
        for (name, tree) in &self.directories {
            let response = egui::CollapsingHeader::new(name.to_string_lossy())
                .id_salt((&c.repo, name))
                .default_open(true)
                .show(ui, |ui| tree.show(ui, c, selected, action, busy));
            if c.working_tree {
                let indices = tree.file_indices();
                let conflicted = indices.iter().any(|&i| c.files[i].status == "U");
                let staged = indices.first().is_some_and(|&i| c.files[i].staged);
                let header = response.header_response.on_hover_text(if conflicted {
                    "Resolve merge conflicts in this directory before staging or unstaging"
                } else if staged {
                    "Double-click to unstage all changed files in this directory"
                } else {
                    "Double-click to stage all changed files in this directory"
                });
                if header.double_clicked() && !busy && !conflicted {
                    *action = Some(indices);
                }
            }
        }
        for (name, index) in &self.files {
            let file = &c.files[*index];
            let status = file.status.chars().next().unwrap_or('M');
            let color = match status {
                'A' => GREEN,
                'D' => RED,
                'R' => egui::Color32::from_rgb(140, 180, 245),
                _ => egui::Color32::from_rgb(222, 189, 120),
            };
            let label =
                egui::RichText::new(format!("{status}  {}", name.to_string_lossy())).color(color);
            let response = ui
                .selectable_label(*selected == Some(*index), label)
                .on_hover_text(file.path.to_string_lossy());
            if response.clicked() {
                *selected = Some(*index);
            }
            if c.working_tree {
                let conflicted = file.status == "U";
                let response = response.on_hover_text(if conflicted {
                    "Resolve merge conflicts before staging or unstaging"
                } else if file.staged {
                    "Double-click to unstage this file"
                } else {
                    "Double-click to stage this entire file"
                });
                if response.double_clicked() && !busy && !conflicted {
                    *action = Some(vec![*index]);
                }
            }
        }
    }
}

struct Viewer {
    events: mpsc::Receiver<Event>,
    requests: mpsc::Sender<Request>,
    repo_input: String,
    repo: std::path::PathBuf,
    generation: u64,
    target: git::Target,
    history: Option<History>,
    history_error: Option<String>,
    history_limit: usize,
    branch_filter: String,
    commit_filter: String,
    loading: bool,
    refresh_pending: bool,
    refresh_error: Option<String>,
    next_refresh: Instant,
    was_focused: bool,
    history_source: Option<String>,
    refs: Option<Vec<u8>>,
    latest_generation: Arc<AtomicU64>,
    comparison: Option<Arc<Comparison>>,
    tree: Tree,
    staged_tree: Tree,
    staging_pending: bool,
    staging_error: Option<String>,
    follow_file: Option<(std::path::PathBuf, bool)>,
    error: Option<String>,
    selected: Option<usize>,
    cache: HashMap<usize, Result<Arc<Prepared>, String>>,
    requested: std::collections::HashSet<usize>,
    split: bool,
    full: bool,
    light_mode: bool,
    code_zoom: f32,
    scroll_offsets: HashMap<(egui::Id, bool, bool), f32>,
    pending_scroll: Option<((egui::Id, bool, bool), f32)>,
}
impl Viewer {
    fn start(options: cli::Options) -> Self {
        let (events_tx, events) = mpsc::channel();
        let (requests, requests_rx) = mpsc::channel();
        let repo_input = project_path_for_input(&options.repo);
        let latest_generation = Arc::new(AtomicU64::new(0));
        let worker_generation = latest_generation.clone();
        thread::spawn(move || {
            while let Ok(request) = requests_rx.recv() {
                match request {
                    Request::Refresh(request) => {
                        if worker_generation.load(Ordering::Relaxed) != request.generation {
                            continue;
                        }
                        let result = refresh(&request);
                        let _ = events_tx.send(Event::Refreshed(
                            request.generation,
                            request.selected,
                            result,
                        ));
                    }
                    Request::ToggleStaging(generation, repo, files) => {
                        let result = git::toggle_staging_files(&repo, &files);
                        if let Some(file) = files.into_iter().next() {
                            let _ = events_tx.send(Event::Staging(generation, file, result));
                        }
                    }
                    Request::Reload {
                        generation,
                        repo,
                        mut target,
                        source,
                        limit,
                        select_tip,
                    } => {
                        let history = read_history(&repo, source, limit);
                        if select_tip {
                            target = history
                                .as_ref()
                                .ok()
                                .and_then(|h| h.commits.first())
                                .map(|c| git::Target::Commit(c.id.clone()))
                                .unwrap_or(git::Target::Worktree);
                        }
                        let result = if select_tip && history.is_err() {
                            Err(history.as_ref().err().unwrap().clone())
                        } else {
                            git::compare(&repo, target.clone()).map(Arc::new)
                        };
                        let _ = events_tx.send(Event::History(generation, history));
                        if events_tx
                            .send(Event::Comparison(generation, target, result))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Request::File(generation, index, comparison) => {
                        if worker_generation.load(Ordering::Relaxed) != generation {
                            continue;
                        }
                        let result = git::load_file(&comparison, index)
                            .map(Prepared::new)
                            .map(Arc::new);
                        if events_tx
                            .send(Event::File(generation, index, result))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        let source = match &options.target {
            git::Target::Branch(name) => Some(if name.starts_with("refs/") {
                name.clone()
            } else {
                format!("refs/heads/{name}")
            }),
            _ => None,
        };
        let _ = requests.send(Request::Reload {
            generation: 0,
            repo: options.repo.clone(),
            target: options.target.clone(),
            source: source.clone(),
            limit: 100,
            select_tip: false,
        });
        Self {
            events,
            requests,
            repo_input,
            repo: options.repo,
            generation: 0,
            target: options.target,
            history: None,
            history_error: None,
            history_limit: 100,
            branch_filter: String::new(),
            commit_filter: String::new(),
            loading: true,
            refresh_pending: false,
            refresh_error: None,
            next_refresh: Instant::now(),
            was_focused: false,
            history_source: source,
            refs: None,
            latest_generation,
            comparison: None,
            tree: Tree::default(),
            staged_tree: Tree::default(),
            staging_pending: false,
            staging_error: None,
            follow_file: None,
            error: None,
            selected: None,
            cache: HashMap::new(),
            requested: Default::default(),
            split: false,
            full: false,
            light_mode: false,
            code_zoom: 1.0,
            scroll_offsets: HashMap::new(),
            pending_scroll: None,
        }
    }
    fn reload(&mut self, source: Option<String>, select_tip: bool) {
        self.generation += 1;
        self.latest_generation
            .store(self.generation, Ordering::Relaxed);
        self.refresh_pending = false;
        self.refresh_error = None;
        self.refs = None;
        self.next_refresh = Instant::now() + Duration::from_secs(1);
        self.loading = true;
        self.comparison = None;
        self.tree = Tree::default();
        self.staged_tree = Tree::default();
        self.follow_file = None;
        self.selected = None;
        self.cache.clear();
        self.requested.clear();
        self.scroll_offsets.clear();
        self.pending_scroll = None;
        self.error = None;
        self.staging_error = None;
        self.history_error = None;
        let _ = self.requests.send(Request::Reload {
            generation: self.generation,
            repo: self.repo.clone(),
            target: self.target.clone(),
            source,
            limit: self.history_limit,
            select_tip,
        });
    }

    fn toggle_staging(&mut self, indices: &[usize]) {
        if self.staging_pending || self.refresh_error.is_some() {
            return;
        }
        let Some(comparison) = &self.comparison else {
            return;
        };
        if !comparison.working_tree {
            return;
        }
        let Some(mut files) = indices
            .iter()
            .map(|&index| comparison.files.get(index).cloned())
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        if files.is_empty() || files.iter().any(|file| file.status == "U") {
            return;
        }
        if let Some(selected) = self.selected.and_then(|index| comparison.files.get(index)) {
            if let Some(position) = files
                .iter()
                .position(|file| file.path == selected.path && file.staged == selected.staged)
            {
                files.swap(0, position);
            }
        }
        self.generation += 1;
        self.latest_generation
            .store(self.generation, Ordering::Relaxed);
        self.refresh_pending = false;
        self.requested.clear();
        self.staging_error = None;
        match self.requests.send(Request::ToggleStaging(
            self.generation,
            comparison.repo.clone(),
            files,
        )) {
            Ok(()) => self.staging_pending = true,
            Err(_) => self.staging_error = Some("Repository worker is unavailable".into()),
        }
    }

    fn selected_key(&self) -> Option<FileKey> {
        self.comparison
            .as_ref()
            .and_then(|c| self.selected.and_then(|i| c.files.get(i)))
            .map(file_key)
    }

    fn schedule_refresh(&mut self, focused: bool, now: Instant) {
        if focused && !self.was_focused {
            self.next_refresh = now;
        }
        self.was_focused = focused;
        if focused && now >= self.next_refresh {
            self.start_refresh(now);
        }
    }

    fn start_refresh(&mut self, now: Instant) {
        if self.loading || self.staging_pending || self.refresh_pending {
            return;
        }
        self.next_refresh = now + Duration::from_secs(1);
        let Some(comparison) = self.comparison.clone() else {
            return;
        };
        let request = RefreshRequest {
            generation: self.generation,
            comparison,
            target: self.target.clone(),
            selected: self.selected_key(),
            source: self.history_source.clone(),
            limit: self.history_limit,
            refs: self.refs.clone(),
        };
        if self.requests.send(Request::Refresh(request)).is_ok() {
            self.refresh_pending = true;
        } else {
            self.refresh_error = Some("Repository worker is unavailable".into());
        }
    }

    fn accept_refresh(
        &mut self,
        generation: u64,
        selected: Option<FileKey>,
        result: Result<RefreshResult, String>,
    ) {
        if generation != self.generation {
            return;
        }
        self.refresh_pending = false;
        self.next_refresh = Instant::now() + Duration::from_secs(1);
        if selected != self.selected_key() {
            return;
        }
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.refresh_error = Some(error);
                return;
            }
        };
        self.refresh_error = None;
        if let Some(history) = result.history {
            match history {
                Ok(history) => {
                    self.history = Some(history);
                    self.history_error = None;
                    self.refs = Some(result.refs);
                }
                Err(error) => {
                    self.history_error = Some(error);
                    self.refs = None;
                    if let (Some(history), Some(branches)) =
                        (&mut self.history, result.branches_on_history_error)
                    {
                        history.branches = branches;
                        history.commits.clear();
                        history.has_more = false;
                    }
                }
            }
        } else {
            self.refs = Some(result.refs);
        }
        let c = result.comparison;
        let changed = self.comparison.as_deref() != Some(c.as_ref());
        if changed {
            self.tree = Tree::new(&c, false);
            self.staged_tree = Tree::new(&c, true);
        }
        let old_cache = self.selected.and_then(|i| self.cache.get(&i)).cloned();
        self.generation += 1;
        self.latest_generation
            .store(self.generation, Ordering::Relaxed);
        self.requested.clear();
        if c.working_tree || changed {
            self.cache.clear();
        }
        self.selected = result.selected.as_ref().map(|(index, _)| *index);
        if let Some((index, prepared)) = result.selected {
            let prepared = match old_cache {
                Some(Ok(old)) if old.diff == prepared.diff => old,
                _ => prepared,
            };
            self.cache.insert(index, Ok(prepared));
            self.requested.insert(index);
            let key = (file_id(&c, index), self.split, self.full);
            // Follow the same path when external staging moves it between sections.
            let offset = self.scroll_offsets.get(&key).copied().or_else(|| {
                let old = self.comparison.as_ref()?;
                let old_index = selected.as_ref().and_then(|key| find_file(old, key))?;
                self.scroll_offsets
                    .get(&(file_id(old, old_index), self.split, self.full))
                    .copied()
            });
            if let Some(offset) = offset {
                self.pending_scroll = Some((key, offset));
            }
        }
        self.comparison = Some(c);
    }

    fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.accept(event);
        }
        if let Some(index) = self.selected {
            if self.requested.insert(index) {
                if let Some(c) = &self.comparison {
                    let _ = self
                        .requests
                        .send(Request::File(self.generation, index, c.clone()));
                }
            }
        }
    }

    fn accept(&mut self, event: Event) {
        match event {
            Event::Refreshed(generation, selected, result) => {
                self.accept_refresh(generation, selected, result)
            }
            Event::Staging(generation, file, result) => {
                self.staging_pending = false;
                if generation == self.generation {
                    match result {
                        Ok(()) => {
                            let source = self.history_source.clone();
                            self.reload(source, false);
                            self.follow_file = Some((file.path, !file.staged));
                        }
                        Err(error) => self.staging_error = Some(error),
                    }
                }
            }
            Event::History(generation, result) if generation == self.generation => match result {
                Ok(history) => self.history = Some(history),
                Err(error) => {
                    self.history = None;
                    self.history_error = Some(error);
                }
            },
            Event::Comparison(generation, target, result) if generation == self.generation => {
                self.loading = false;
                self.target = target;
                match result {
                    Ok(c) => {
                        if matches!(self.target, git::Target::Commit(_)) {
                            self.target = git::Target::Commit(c.new.clone());
                        }
                        self.tree = Tree::new(&c, false);
                        self.staged_tree = Tree::new(&c, true);
                        self.selected = if let Some((path, staged)) = self.follow_file.take() {
                            c.files
                                .iter()
                                .position(|f| f.path == path && f.staged == staged)
                                .or_else(|| c.files.iter().position(|f| f.path == path))
                        } else {
                            (!c.files.is_empty()).then_some(0)
                        };
                        self.comparison = Some(c);
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Event::File(generation, index, result) if generation == self.generation => {
                self.cache.insert(index, result);
            }
            _ => {}
        }
    }
}

const CODE_ROW_HEIGHT: f32 = 16.0;

const GREEN: egui::Color32 = egui::Color32::from_rgb(135, 218, 165);
const RED: egui::Color32 = egui::Color32::from_rgb(244, 150, 152);

fn main() {
    let options = match cli::parse(std::env::args_os().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{}", cli::HELP);
            return;
        }
        Err(error) => {
            eprintln!("{error}\n\n{}", cli::HELP);
            std::process::exit(2);
        }
    };
    App::new()
        .insert_non_send_resource(Viewer::start(options))
        .insert_resource(WinitSettings::desktop_app())
        .add_plugins(
            DefaultPlugins
                .set(bevy::log::LogPlugin {
                    filter: "off".into(),
                    ..default()
                })
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Differ".into(),
                        resolution: (1280, 800).into(),
                        ..default()
                    }),
                    ..default()
                }),
        )
        .add_plugins(EguiPlugin::default())
        .add_systems(Startup, |mut commands: Commands| {
            commands.spawn(Camera2d);
        })
        .add_systems(EguiPrimaryContextPass, draw)
        .run();
}

fn home_directory() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::path::MAIN_SEPARATOR.to_string()))
}

fn project_path_for_input(path: &std::path::Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let home = home_directory();
    absolute
        .strip_prefix(&home)
        .unwrap_or(&absolute)
        .to_string_lossy()
        .into_owned()
}

fn project_path_from_input(path: &std::path::Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        home_directory().join(path)
    }
}

fn delete_text_before_cursor(text: &mut String, range: Option<egui::text::CCursorRange>) {
    let char_count = text.chars().count();
    let selected = range
        .map(|range| range.as_sorted_char_range())
        .unwrap_or(char_count..char_count);
    let (start, end) = if selected.is_empty() {
        if selected.start == 0 {
            return;
        }
        (selected.start - 1, selected.start)
    } else {
        (selected.start, selected.end)
    };
    let byte_start = text
        .char_indices()
        .nth(start)
        .map_or(text.len(), |(byte, _)| byte);
    let byte_end = text
        .char_indices()
        .nth(end)
        .map_or(text.len(), |(byte, _)| byte);
    text.replace_range(byte_start..byte_end, "");
}

/// Keep editing and clearing a filter inside the popup; only choosing an item closes it.
fn picker_filter(ui: &mut egui::Ui, query: &mut String, hint: &str, focus: bool) {
    ui.horizontal(|ui| {
        let response = ui.add(
            egui::TextEdit::singleline(query)
                .hint_text(hint)
                .desired_width(230.0),
        );
        if focus {
            response.request_focus();
        }
        if ui.small_button("×").on_hover_text("Clear filter").clicked() {
            query.clear();
            response.request_focus();
        }
    });
    ui.separator();
}

/// Reserve five result rows plus the filter and other popup controls. The
/// scroll area still shrinks to its contents when fewer entries are available.
fn picker_height(ui: &egui::Ui, extra_rows: usize) -> f32 {
    let row_height =
        ui.spacing().interact_size.y.max(
            ui.text_style_height(&egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.y,
        ) + ui.spacing().item_spacing.y;
    // Filter, separator, and a little breathing room above the results.
    ((8 + extra_rows) as f32 * row_height).max(ui.spacing().combo_height)
}

fn draw(mut contexts: EguiContexts, mut viewer: NonSendMut<Viewer>) -> Result {
    let ctx = contexts.ctx_mut()?;
    viewer.poll();
    let focused = ctx.input(|input| input.raw.focused);
    viewer.schedule_refresh(focused, Instant::now());
    if focused {
        ctx.request_repaint_after(Duration::from_millis(250));
    }
    let copy_path_shortcut = ctx.input(|input| {
        input.modifiers.ctrl && input.modifiers.shift && input.key_pressed(egui::Key::C)
    });
    if copy_path_shortcut {
        if let (Some(comparison), Some(index)) = (&viewer.comparison, viewer.selected) {
            if let Some(file) = comparison.files.get(index) {
                ctx.copy_text(file.path.to_string_lossy().into_owned());
            }
        }
    }
    let zoom_shortcut = ctx.input(|input| {
        if !input.modifiers.ctrl {
            return None;
        }
        if input.key_pressed(egui::Key::Plus) {
            Some(true)
        } else if input.key_pressed(egui::Key::Minus) {
            Some(false)
        } else {
            None
        }
    });
    if let Some(zoom_in) = zoom_shortcut {
        let scroll_key = viewer.selected.and_then(|index| {
            viewer
                .comparison
                .as_ref()
                .map(|c| (file_id(c, index), viewer.split, viewer.full))
        });
        adjust_code_zoom(&mut viewer, zoom_in, scroll_key);
    }
    ctx.set_visuals(if viewer.light_mode {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    });
    if viewer.staging_pending
        || viewer.refresh_pending
        || viewer.loading
        || (viewer.comparison.is_none() && viewer.error.is_none())
        || viewer
            .selected
            .is_some_and(|i| !viewer.cache.contains_key(&i))
    {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
    egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let scroll_key = viewer
                .selected
                .and_then(|index| viewer.comparison.as_ref().map(|c| (file_id(c, index), viewer.split, viewer.full)));
            ui.strong("DIFF");
            ui.separator();
            ui.selectable_value(&mut viewer.split, false, "Inline");
            ui.selectable_value(&mut viewer.split, true, "Side by side");
            ui.separator();
            ui.selectable_value(&mut viewer.full, false, "Changes");
            ui.selectable_value(&mut viewer.full, true, "Full file");
            ui.separator();
            let theme_label = if viewer.light_mode {
                "☾ Dark"
            } else {
                "☀ Light"
            };
            if ui
                .button(theme_label)
                .on_hover_text("Switch between light and dark themes")
                .clicked()
            {
                viewer.light_mode = !viewer.light_mode;
            }
            ui.separator();
            if ui
                .button("⟳ Reload")
                .on_hover_text("Reload changes from the repository")
                .clicked()
            {
                if viewer.comparison.is_some() {
                    viewer.refs = None;
                    viewer.start_refresh(Instant::now());
                } else {
                    let source = viewer.history_source.clone();
                    viewer.reload(source, false);
                }
            }
            ui.separator();
            if ui
                .add_enabled(viewer.code_zoom > 0.5, egui::Button::new("A−"))
                .on_hover_text("Zoom code out (Ctrl+-)")
                .clicked()
            {
                adjust_code_zoom(&mut viewer, false, scroll_key);
            }
            ui.label(format!("{:.0}%", viewer.code_zoom * 100.0));
            if ui
                .add_enabled(viewer.code_zoom < 2.0, egui::Button::new("A+"))
                .on_hover_text("Zoom code in (Ctrl++)")
                .clicked()
            {
                adjust_code_zoom(&mut viewer, true, scroll_key);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Project");
            let path_before_edit = viewer.repo_input.clone();
            let path_response = ui
                .add(
                    egui::TextEdit::singleline(&mut viewer.repo_input)
                        .id(egui::Id::new("project_path"))
                        .desired_width(300.0)
                        .hint_text("path relative to $HOME or absolute"),
                )
                .on_hover_text("Edit a path relative to $HOME or enter an absolute path");
            if path_response.has_focus()
                && ui.input(|input| input.key_pressed(egui::Key::Backspace))
                && viewer.repo_input == path_before_edit
            {
                let range = egui::widgets::text_edit::TextEditState::load(ctx, path_response.id)
                    .and_then(|state| state.cursor.char_range());
                delete_text_before_cursor(&mut viewer.repo_input, range);
            }
            let mut selected_folder = None;
            if ui.button("Browse…").clicked() {
                let directory =
                    project_path_from_input(std::path::Path::new(viewer.repo_input.trim()));
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Select project folder")
                    .set_directory(directory)
                    .pick_folder()
                {
                    viewer.repo_input = project_path_for_input(&path);
                    selected_folder = Some(path);
                }
            }
            let open_clicked = ui
                .add_enabled(
                    !viewer.repo_input.trim().is_empty(),
                    egui::Button::new("Open"),
                )
                .on_hover_text("Open this repository")
                .clicked();
            let enter_pressed =
                path_response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let repo_to_open = selected_folder.or_else(|| {
                (!viewer.repo_input.trim().is_empty() && (open_clicked || enter_pressed)).then(
                    || project_path_from_input(std::path::Path::new(viewer.repo_input.trim())),
                )
            });
            if let Some(repo) = repo_to_open {
                viewer.branch_filter.clear();
                viewer.commit_filter.clear();
                viewer.repo = repo;
                viewer.target = git::Target::Worktree;
                viewer.history_source = None;
                viewer.history = None;
                viewer.history_limit = 100;
                viewer.reload(None, false);
            }
        });
        let mut branch_choice = None;
        let mut target_choice = None;
        let mut load_more = false;
        let mut branch_filter = std::mem::take(&mut viewer.branch_filter);
        let mut commit_filter = std::mem::take(&mut viewer.commit_filter);
        ui.horizontal_wrapped(|ui| {
            if let Some(history) = &viewer.history {
                ui.label("Branch");
                let branch_label = history.source.strip_prefix("refs/heads/").unwrap_or(&history.source);
                let branch_label = if history.source == "HEAD" {
                    history.branches.current.as_deref().unwrap_or("Detached HEAD")
                } else { branch_label };
                let branch_was_open = egui::ComboBox::is_open(ui.ctx(), ui.make_persistent_id("branch_selector"));
                egui::ComboBox::from_id_salt("branch_selector").selected_text(branch_label)
                    .height(picker_height(ui, 0))
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show_ui(ui, |ui| {
                        picker_filter(ui, &mut branch_filter, "Filter branches…", !branch_was_open);
                        let query = view::fold_filter_text(&branch_filter);
                        let mut matches = 0;
                        for branch in &history.branches.local {
                            if !view::filter_matches(&query, &[branch]) { continue; }
                            matches += 1;
                            let source = format!("refs/heads/{branch}");
                            if ui.selectable_label(history.source == source, branch).clicked() {
                                branch_choice = Some(source);
                                ui.close();
                            }
                        }
                        if matches == 0 { ui.weak("No matching branches"); }
                    });
                ui.label("Commit");
                let selected_label = match &viewer.target {
                    git::Target::Worktree => "Working-tree changes (current checkout)".to_owned(),
                    git::Target::Branch(name) => format!("HEAD → {name}"),
                    git::Target::Commit(id) => history.commits.iter().find(|c| &c.id == id)
                        .map(|c| format!("{} — {}", c.short_id, c.subject))
                        .unwrap_or_else(|| format!("Commit {id}")),
                };
                let commit_was_open = egui::ComboBox::is_open(ui.ctx(), ui.make_persistent_id("commit_selector"));
                egui::ComboBox::from_id_salt("commit_selector").width(350.0)
                    .height(picker_height(ui, 3))
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .selected_text(selected_label).show_ui(ui, |ui| {
                        picker_filter(ui, &mut commit_filter, "Filter commits…", !commit_was_open);
                        ui.weak(format!("Filtering {} loaded commits", history.commits.len()));
                        if ui.selectable_label(matches!(viewer.target, git::Target::Worktree),
                            "Working-tree changes (current checkout)").clicked() {
                            target_choice = Some(git::Target::Worktree);
                            ui.close();
                        }
                        let query = view::fold_filter_text(&commit_filter);
                        let mut matches = 0;
                        for commit in &history.commits {
                            if !view::filter_matches(&query, &[&commit.id, &commit.short_id, &commit.subject]) { continue; }
                            matches += 1;
                            if ui.selectable_label(matches!(&viewer.target, git::Target::Commit(id) if id == &commit.id),
                                format!("{} — {}", commit.short_id, commit.subject)).clicked() {
                                target_choice = Some(git::Target::Commit(commit.id.clone()));
                                ui.close();
                            }
                        }
                        if history.commits.is_empty() { ui.label("No commits yet"); }
                        else if matches == 0 { ui.weak("No matching loaded commits"); }
                        if history.has_more && ui.add_enabled(!viewer.loading, egui::Button::new("Load more…")).clicked() {
                            load_more = true;
                            ui.close();
                        }
                    });
            }
            if viewer.loading { ui.spinner(); ui.label("Loading…"); }
            if let Some(error) = &viewer.refresh_error { ui.colored_label(RED, format!("Auto-refresh failed: {error}")); }
            if let Some(error) = &viewer.history_error { ui.colored_label(RED, error); }
        });
        viewer.branch_filter = branch_filter;
        viewer.commit_filter = commit_filter;
        if let Some(source) = branch_choice {
            viewer.history_source = Some(source.clone());
            viewer.history_limit = 100;
            viewer.reload(Some(source), true);
        } else if let Some(target) = target_choice {
            viewer.target = target;
            let source = viewer.history_source.clone();
            viewer.reload(source, false);
        } else if load_more {
            viewer.history_limit += 100;
            let source = viewer.history_source.clone();
            viewer.reload(source, false);
        }
        if let Some(c) = viewer.comparison.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(c.repo.to_string_lossy()).weak());
                ui.separator();
                ui.label(&c.label);
            });
        }
        ui.add_space(4.0);
    });
    if let Some(c) = viewer.comparison.clone() {
        egui::SidePanel::left("files")
            .default_width(260.0)
            .width_range(140.0..=600.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.strong(format!("Changed files  ·  {}", c.files.len()));
                ui.separator();
                let mut selected = viewer.selected;
                let mut action = None;
                egui::ScrollArea::both().show(ui, |ui| {
                    if c.working_tree {
                        for staged in [true, false] {
                            let count = c.files.iter().filter(|f| f.staged == staged).count();
                            ui.push_id(staged, |ui| {
                                egui::CollapsingHeader::new(format!(
                                    "{} · {count}",
                                    if staged { "Staged" } else { "Unstaged" }
                                ))
                                .id_salt(("staging_section", staged))
                                .default_open(true)
                                .show(ui, |ui| {
                                    let tree = if staged {
                                        &viewer.staged_tree
                                    } else {
                                        &viewer.tree
                                    };
                                    tree.show(
                                        ui,
                                        &c,
                                        &mut selected,
                                        &mut action,
                                        viewer.staging_pending || viewer.refresh_error.is_some(),
                                    );
                                    if count == 0 {
                                        ui.weak("No files");
                                    }
                                });
                            });
                        }
                    } else {
                        viewer.tree.show(ui, &c, &mut selected, &mut action, true);
                    }
                });
                if let Some(index) = action {
                    viewer.toggle_staging(&index);
                }
                if viewer.staging_pending {
                    ui.spinner();
                    ui.label("Updating staging area…");
                }
                if let Some(error) = &viewer.staging_error {
                    ui.colored_label(RED, error);
                }
                if viewer.selected != selected {
                    ctx.request_repaint();
                }
                viewer.selected = selected;
            });
    }
    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(error) = &viewer.error {
            ui.heading("Could not open comparison");
            ui.colored_label(RED, error);
            return;
        }
        let Some(c) = viewer.comparison.clone() else {
            ui.spinner();
            ui.label("Reading repository…");
            return;
        };
        let Some(index) = viewer.selected else {
            if !c.files.is_empty() {
                ui.heading("Select a file");
                return;
            }
            ui.heading("No changes");
            ui.label(if c.working_tree {
                "No staged or unstaged changes."
            } else {
                "These revisions have identical file trees."
            });
            return;
        };
        let file = &c.files[index];
        ui.heading(file.path.to_string_lossy());
        if c.working_tree {
            ui.weak(if file.staged {
                "Staged · HEAD → index"
            } else {
                "Unstaged · index → working tree"
            });
        }
        if file.path != file.old_path {
            ui.label(format!("Renamed from {}", file.old_path.display()));
        }
        ui.separator();
        let Some(result) = viewer.cache.get(&index).cloned() else {
            ui.spinner();
            ui.label("Loading diff…");
            return;
        };
        let prepared = match result {
            Ok(p) => p,
            Err(error) => {
                ui.colored_label(RED, error);
                return;
            }
        };
        if let Some(summary) = &prepared.diff.summary {
            ui.label(summary);
        }
        if prepared.diff.rows.is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("− removed").color(RED));
            ui.label(egui::RichText::new("+ added").color(GREEN));
            ui.separator();
            if ui.small_button("Copy path (Ctrl+Shift+C)").clicked() {
                ui.ctx().copy_text(file.path.to_string_lossy().into_owned());
            }
            ui.separator();
            if ui.small_button("Copy old file").clicked() {
                ui.ctx()
                    .copy_text(view::file_text(&prepared.diff.rows, false));
            }
            if ui.small_button("Copy new file").clicked() {
                ui.ctx()
                    .copy_text(view::file_text(&prepared.diff.rows, true));
            }
        });
        let split = viewer.split;
        let rows = &prepared.layouts[usize::from(split) + 2 * usize::from(viewer.full)];
        let zoom = viewer.code_zoom;
        let row_height = CODE_ROW_HEIGHT * zoom;
        let full = viewer.full;
        let show_change_marks = full && rows.len() as f32 * row_height > ui.available_height();
        let marks_width = if show_change_marks { 8.0 } else { 0.0 };
        let viewport_width = (ui.available_width() - marks_width).max(1.0);
        let content_width = (prepared.width as f32 * 8.5 + 140.0) * zoom;
        let width = if split {
            ((viewport_width - 24.0) / 2.0).max(100.0)
        } else {
            content_width.max(viewport_width)
        };
        let mut horizontal_offset = 0.0;
        if split {
            ui.columns(2, |cols| {
                cols[0].label("OLD");
                cols[1].label("NEW");
            });
            // Keep both columns visible; one horizontal scrollbar moves their code together.
            if content_width > width {
                horizontal_offset = egui::ScrollArea::horizontal()
                    .id_salt((file_id(&c, index), "code-horizontal"))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.allocate_space(egui::vec2(viewport_width + content_width - width, 2.0));
                    })
                    .state
                    .offset
                    .x;
            }
        }
        let scroll = if split {
            egui::ScrollArea::vertical()
        } else {
            egui::ScrollArea::both()
        };
        let scroll_key = (file_id(&c, index), split, full);
        let max_scroll = (rows.len() as f32 * row_height - ui.available_height()).max(0.0);
        let restore_offset = viewer
            .pending_scroll
            .filter(|(key, _)| *key == scroll_key)
            .map(|(_, offset)| clamp_scroll_offset(offset, max_scroll));
        if restore_offset.is_some() {
            viewer.pending_scroll = None;
        }
        let scroll = scroll
            .id_salt(scroll_key)
            .auto_shrink([false, false])
            .scroll_bar_visibility(if show_change_marks {
                egui::scroll_area::ScrollBarVisibility::AlwaysVisible
            } else {
                egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded
            });
        let scroll = if let Some(offset) = restore_offset {
            scroll.vertical_scroll_offset(offset)
        } else {
            scroll
        };
        let output = if show_change_marks {
            let viewport_height = ui.available_height();
            ui.horizontal(|ui| {
                let scroll_width = (ui.available_width() - marks_width).max(1.0);
                let output = ui
                    .allocate_ui_with_layout(
                        egui::vec2(scroll_width, viewport_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |scroll_ui| {
                            let original_style = scroll_ui.spacing().scroll;
                            let mut overview_style = original_style;
                            overview_style.floating = false;
                            overview_style.dormant_background_opacity = 0.25;
                            overview_style.dormant_handle_opacity = 0.65;
                            scroll_ui.spacing_mut().scroll = overview_style;
                            let output = scroll.show_rows(
                                scroll_ui,
                                row_height,
                                rows.len(),
                                |scroll_ui, range| {
                                    render_code_rows(
                                        scroll_ui,
                                        range,
                                        rows,
                                        split,
                                        width,
                                        row_height,
                                        &prepared,
                                        horizontal_offset,
                                        zoom,
                                    );
                                },
                            );
                            scroll_ui.spacing_mut().scroll = original_style;
                            output
                        },
                    )
                    .inner;
                let (marks_rect, _) = ui.allocate_exact_size(
                    egui::vec2(marks_width, output.inner_rect.height()),
                    egui::Sense::hover(),
                );
                paint_change_overview(ui, marks_rect, rows, &prepared.diff);
                output
            })
            .inner
        } else {
            scroll.show_rows(ui, row_height, rows.len(), |scroll_ui, range| {
                render_code_rows(
                    scroll_ui,
                    range,
                    rows,
                    split,
                    width,
                    row_height,
                    &prepared,
                    horizontal_offset,
                    zoom,
                );
            })
        };
        viewer
            .scroll_offsets
            .insert(scroll_key, output.state.offset.y);
    });
    viewer.pending_scroll = None;
    Ok(())
}

fn adjust_code_zoom(
    viewer: &mut Viewer,
    zoom_in: bool,
    scroll_key: Option<(egui::Id, bool, bool)>,
) {
    let old_zoom = viewer.code_zoom;
    let new_zoom = if zoom_in {
        (old_zoom + 0.1).min(2.0)
    } else {
        (old_zoom - 0.1).max(0.5)
    };
    if (new_zoom - old_zoom).abs() < f32::EPSILON {
        return;
    }

    let old_offset = scroll_key
        .and_then(|key| viewer.scroll_offsets.get(&key).copied())
        .unwrap_or(0.0);
    viewer.code_zoom = new_zoom;
    viewer.pending_scroll = scroll_key.map(|key| {
        (
            key,
            preserve_scroll_anchor(
                old_offset,
                CODE_ROW_HEIGHT * old_zoom,
                CODE_ROW_HEIGHT * new_zoom,
            ),
        )
    });
}

fn render_code_rows(
    ui: &mut egui::Ui,
    range: std::ops::Range<usize>,
    rows: &[view::DisplayRow],
    split: bool,
    width: f32,
    row_height: f32,
    prepared: &Prepared,
    horizontal_offset: f32,
    zoom: f32,
) {
    ui.set_min_width(if split { width * 2.0 + 12.0 } else { width });
    for row_index in range {
        let row = &rows[row_index];
        ui.push_id(row_index, |ui| {
            if let Some(count) = row.gap {
                ui.add_sized(
                    [width, row_height],
                    egui::Label::new(
                        egui::RichText::new(format!("··· {count} unchanged lines ···")).weak(),
                    )
                    .truncate(),
                );
            } else if split {
                let row_size = egui::vec2(width * 2.0 + 12.0, row_height);
                let (row_rect, _) = ui.allocate_exact_size(row_size, egui::Sense::hover());
                let left_rect =
                    egui::Rect::from_min_size(row_rect.min, egui::vec2(width, row_height));
                let right_rect = egui::Rect::from_min_size(
                    egui::pos2(row_rect.left() + width + 12.0, row_rect.top()),
                    egui::vec2(width, row_height),
                );
                cell(
                    ui,
                    &prepared.diff,
                    row.left,
                    false,
                    false,
                    left_rect,
                    horizontal_offset,
                    zoom,
                );
                cell(
                    ui,
                    &prepared.diff,
                    row.right,
                    true,
                    false,
                    right_rect,
                    horizontal_offset,
                    zoom,
                );
            } else {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, row_height), egui::Sense::hover());
                cell(ui, &prepared.diff, row.left, false, true, rect, 0.0, zoom);
            }
        });
    }
}

fn paint_change_overview(
    ui: &egui::Ui,
    rect: egui::Rect,
    rows: &[view::DisplayRow],
    diff: &FileDiff,
) {
    if rows.is_empty() || rect.height() <= 0.0 {
        return;
    }
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().widgets.noninteractive.bg_fill);
    for (row_index, row) in rows.iter().enumerate() {
        let mut removed = false;
        let mut added = false;
        for line_index in [row.left, row.right].into_iter().flatten() {
            match diff.rows.get(line_index).map(|line| line.kind) {
                Some(LineKind::Removed) => removed = true,
                Some(LineKind::Added) => added = true,
                _ => {}
            }
        }
        if !removed && !added {
            continue;
        }
        let y = rect.top() + ((row_index as f32 + 0.5) / rows.len() as f32) * rect.height();
        let mark_height = 3.0_f32.min(rect.height());
        if removed && added {
            let half = rect.width() / 2.0;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left(), y - mark_height / 2.0),
                    egui::vec2(half, mark_height),
                ),
                0.0,
                RED,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left() + half, y - mark_height / 2.0),
                    egui::vec2(rect.width() - half, mark_height),
                ),
                0.0,
                GREEN,
            );
        } else {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left(), y - mark_height / 2.0),
                    egui::vec2(rect.width(), mark_height),
                ),
                0.0,
                if removed { RED } else { GREEN },
            );
        }
    }
}

fn preserve_scroll_anchor(offset: f32, old_row_height: f32, new_row_height: f32) -> f32 {
    if !offset.is_finite()
        || !old_row_height.is_finite()
        || !new_row_height.is_finite()
        || old_row_height <= 0.0
        || new_row_height <= 0.0
    {
        return 0.0;
    }
    (offset.max(0.0) / old_row_height) * new_row_height
}

fn clamp_scroll_offset(offset: f32, max_scroll: f32) -> f32 {
    if !offset.is_finite() || !max_scroll.is_finite() {
        return 0.0;
    }
    offset.clamp(0.0, max_scroll.max(0.0))
}

fn cell(
    ui: &mut egui::Ui,
    diff: &FileDiff,
    index: Option<usize>,
    right: bool,
    inline: bool,
    rect: egui::Rect,
    horizontal_offset: f32,
    zoom: f32,
) {
    let Some(index) = index else {
        return;
    };
    let row = &diff.rows[index];
    let dark_mode = ui.visuals().dark_mode;
    let (background, foreground, prefix) = match row.kind {
        LineKind::Added if dark_mode => (egui::Color32::from_rgb(27, 57, 42), GREEN, "+"),
        LineKind::Added => (
            egui::Color32::from_rgb(222, 247, 229),
            egui::Color32::from_rgb(35, 105, 58),
            "+",
        ),
        LineKind::Removed if dark_mode => (egui::Color32::from_rgb(65, 33, 39), RED, "−"),
        LineKind::Removed => (
            egui::Color32::from_rgb(252, 226, 228),
            egui::Color32::from_rgb(155, 45, 55),
            "−",
        ),
        LineKind::Note => (
            if dark_mode {
                egui::Color32::from_rgb(37, 41, 50)
            } else {
                egui::Color32::from_gray(232)
            },
            if dark_mode {
                egui::Color32::from_gray(150)
            } else {
                egui::Color32::from_gray(90)
            },
            " ",
        ),
        LineKind::Context => (
            egui::Color32::TRANSPARENT,
            if dark_mode {
                egui::Color32::from_gray(205)
            } else {
                egui::Color32::from_gray(35)
            },
            " ",
        ),
    };
    ui.painter().rect_filled(rect, 0.0, background);
    let font = egui::FontId::monospace(13.0 * zoom);
    let color = if dark_mode {
        egui::Color32::from_gray(130)
    } else {
        egui::Color32::from_gray(120)
    };
    let gutter_width = (if inline { 120.0 } else { 70.0 }) * zoom;
    let center = rect.center().y;
    if inline {
        ui.painter().text(
            egui::pos2(rect.left() + 42.0 * zoom, center),
            egui::Align2::RIGHT_CENTER,
            row.old.map_or_else(String::new, |n| n.to_string()),
            font.clone(),
            color,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 88.0 * zoom, center),
            egui::Align2::RIGHT_CENTER,
            row.new.map_or_else(String::new, |n| n.to_string()),
            font.clone(),
            color,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 96.0 * zoom, center),
            egui::Align2::LEFT_CENTER,
            prefix,
            font.clone(),
            color,
        );
    } else {
        let line_number = if right { row.new } else { row.old };
        ui.painter().text(
            egui::pos2(rect.left() + 48.0 * zoom, center),
            egui::Align2::RIGHT_CENTER,
            line_number.map_or_else(String::new, |n| n.to_string()),
            font.clone(),
            color,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 56.0 * zoom, center),
            egui::Align2::LEFT_CENTER,
            prefix,
            font.clone(),
            color,
        );
    }
    let text_rect = egui::Rect::from_min_max(rect.min + egui::vec2(gutter_width, 0.0), rect.max);
    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(text_rect.translate(egui::vec2(-horizontal_offset, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    text_ui.set_clip_rect(text_rect.intersect(ui.clip_rect()));
    text_ui.add(
        egui::Label::new(
            egui::RichText::new(row.text.replace('\t', "    "))
                .monospace()
                .size(13.0 * zoom)
                .color(foreground),
        )
        .halign(egui::Align::LEFT)
        .wrap_mode(egui::TextWrapMode::Extend)
        .selectable(true),
    );
}

#[cfg(test)]
mod zoom_tests {
    use super::{clamp_scroll_offset, preserve_scroll_anchor};

    #[test]
    fn zoom_keeps_the_same_row_and_fraction_at_the_top() {
        let old_row_height = 20.0;
        let new_row_height = 18.0;
        let old_offset = 7.25 * old_row_height;
        let new_offset = preserve_scroll_anchor(old_offset, old_row_height, new_row_height);
        assert!((new_offset / new_row_height - 7.25).abs() < 0.0001);
    }

    #[test]
    fn invalid_and_negative_scroll_values_are_safe() {
        assert_eq!(preserve_scroll_anchor(-8.0, 20.0, 18.0), 0.0);
        assert_eq!(preserve_scroll_anchor(f32::NAN, 20.0, 18.0), 0.0);
        assert_eq!(preserve_scroll_anchor(8.0, 0.0, 18.0), 0.0);
        assert_eq!(clamp_scroll_offset(500.0, 120.0), 120.0);
        assert_eq!(clamp_scroll_offset(-2.0, 120.0), 0.0);
        assert_eq!(clamp_scroll_offset(50.0, 0.0), 0.0);
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn directory_indices_include_descendants_only() {
        let tree = Tree {
            files: vec![("root".into(), 0)],
            directories: BTreeMap::from([
                (
                    "dir".into(),
                    Tree {
                        files: vec![("file".into(), 1)],
                        directories: BTreeMap::from([(
                            "nested".into(),
                            Tree {
                                files: vec![("deep".into(), 2)],
                                directories: BTreeMap::new(),
                            },
                        )]),
                    },
                ),
                (
                    "other".into(),
                    Tree {
                        files: vec![("file".into(), 3)],
                        directories: BTreeMap::new(),
                    },
                ),
            ]),
        };
        assert_eq!(
            tree.directories[std::ffi::OsStr::new("dir")].file_indices(),
            vec![1, 2]
        );
    }

    #[test]
    fn staging_is_single_flight_and_stale_completion_does_not_reload() {
        let mut viewer = Viewer::start(cli::Options {
            repo: std::path::PathBuf::from("/nonexistent-differ-test-repository"),
            target: git::Target::Worktree,
        });
        let (requests, received) = mpsc::channel();
        viewer.requests = requests;
        let file = git::ChangedFile {
            path: "file".into(),
            old_path: "file".into(),
            status: "M".into(),
            untracked: false,
            staged: false,
        };
        viewer.comparison = Some(Arc::new(Comparison {
            repo: "/nonexistent-differ-test-repository".into(),
            old: "INDEX".into(),
            new: "WORKTREE".into(),
            label: "test".into(),
            working_tree: true,
            files: vec![file.clone()],
        }));
        viewer.toggle_staging(&[0]);
        viewer.toggle_staging(&[0]);
        assert!(viewer.staging_pending);
        assert!(matches!(
            received.try_recv(),
            Ok(Request::ToggleStaging(1, _, _))
        ));
        assert!(received.try_recv().is_err());
        viewer.generation = 1;
        viewer.accept(Event::Staging(0, file.clone(), Ok(())));
        assert!(!viewer.staging_pending);
        assert_eq!(viewer.generation, 1);
        assert!(received.try_recv().is_err());
        viewer.accept(Event::Staging(1, file.clone(), Err("index locked".into())));
        assert_eq!(viewer.staging_error.as_deref(), Some("index locked"));
        assert!(viewer.comparison.is_some());
        viewer.accept(Event::Staging(1, file, Ok(())));
        assert!(matches!(
            received.try_recv(),
            Ok(Request::Reload { generation: 2, .. })
        ));
        assert_eq!(viewer.follow_file, Some(("file".into(), true)));
    }

    #[test]
    fn changing_selection_discards_all_stale_results_and_resets_view() {
        let mut viewer = Viewer::start(cli::Options {
            repo: std::path::PathBuf::from("/nonexistent-differ-test-repository"),
            target: git::Target::Worktree,
        });
        viewer.selected = Some(7);
        viewer.cache.insert(7, Err("old file".into()));
        viewer
            .scroll_offsets
            .insert((egui::Id::new(7), false, false), 99.0);
        viewer.pending_scroll = Some(((egui::Id::new(7), false, false), 99.0));
        viewer.reload(Some("refs/heads/main".into()), true);
        assert!(viewer.selected.is_none());
        assert!(viewer.cache.is_empty());
        assert!(viewer.scroll_offsets.is_empty());
        assert!(viewer.pending_scroll.is_none());
        viewer.accept(Event::Comparison(
            0,
            git::Target::Commit("old".into()),
            Err("stale".into()),
        ));
        viewer.accept(Event::History(0, Err("stale history".into())));
        viewer.accept(Event::File(0, 7, Err("stale file".into())));
        assert!(viewer.loading);
        assert!(viewer.error.is_none());
        assert!(viewer.history_error.is_none());
        assert!(viewer.cache.is_empty());
        viewer.accept(Event::Comparison(
            viewer.generation,
            git::Target::Worktree,
            Err("current error".into()),
        ));
        assert!(!viewer.loading);
        assert_eq!(viewer.error.as_deref(), Some("current error"));
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use std::{fs, path::PathBuf, process::Command};

    struct Repo(PathBuf);
    impl Repo {
        fn new() -> Self {
            let id = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let repo = Self(
                std::env::temp_dir().join(format!("differ-refresh-{}-{id}", std::process::id())),
            );
            fs::create_dir_all(&repo.0).unwrap();
            repo.git(&["init", "-q"]);
            repo.git(&["config", "user.email", "test@example.com"]);
            repo.git(&["config", "user.name", "Test"]);
            repo
        }
        fn git(&self, args: &[&str]) -> String {
            let output = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().into()
        }
        fn write(&self, path: &str, text: &str) {
            fs::write(self.0.join(path), text).unwrap();
        }
        fn commit(&self) {
            self.git(&["add", "--all"]);
            self.git(&["commit", "-qm", "test"]);
        }
        fn request(&self, selected: Option<FileKey>) -> RefreshRequest {
            RefreshRequest {
                generation: 0,
                comparison: Arc::new(git::compare(&self.0, git::Target::Worktree).unwrap()),
                target: git::Target::Worktree,
                selected,
                source: None,
                limit: 100,
                refs: None,
            }
        }
    }
    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn viewer(c: Arc<Comparison>) -> (Viewer, mpsc::Receiver<Request>) {
        let mut viewer = Viewer::start(cli::Options {
            repo: c.repo.clone(),
            target: git::Target::Worktree,
        });
        let (tx, rx) = mpsc::channel();
        viewer.requests = tx;
        // Ignore initial worker messages: these tests drive events explicitly.
        let (_tx, events) = mpsc::channel();
        viewer.events = events;
        viewer.loading = false;
        viewer.comparison = Some(c);
        viewer.selected = Some(0);
        (viewer, rx)
    }

    #[test]
    fn revision_filters_survive_refresh_and_loading_older_history() {
        let repo = Repo::new();
        repo.write("file", "first");
        repo.commit();
        let older = repo.git(&["rev-parse", "HEAD"]);
        repo.write("file", "second");
        repo.commit();
        repo.write("file", "working change");
        let request = repo.request(Some(("file".into(), false)));
        let (mut viewer, _) = viewer(request.comparison.clone());
        let target = viewer.target.clone();
        let selected = viewer.selected_key();
        viewer.branch_filter = "NO MATCH".into();
        viewer.commit_filter = older.to_uppercase();
        let query = view::fold_filter_text(&viewer.commit_filter);
        let matching = |history: &History| {
            history
                .commits
                .iter()
                .filter(|c| view::filter_matches(&query, &[&c.id, &c.short_id, &c.subject]))
                .count()
        };
        let first = read_history(&repo.0, None, 1).unwrap();
        assert!(first.has_more);
        assert_eq!(matching(&first), 0);
        viewer.history = Some(first);
        // Typing and refresh must leave the chosen comparison intact.
        assert_eq!(viewer.target, target);
        assert_eq!(viewer.selected_key(), selected);
        viewer.accept_refresh(
            viewer.generation,
            request.selected.clone(),
            Ok(refresh(&request).unwrap()),
        );
        assert_eq!(viewer.target, target);
        assert_eq!(viewer.selected_key(), selected);
        assert_eq!(viewer.branch_filter, "NO MATCH");
        assert_eq!(viewer.commit_filter, older.to_uppercase());
        viewer.history_limit = 2;
        viewer.reload(None, false);
        viewer.accept(Event::History(
            viewer.generation,
            read_history(&repo.0, None, 2),
        ));
        assert_eq!(viewer.branch_filter, "NO MATCH");
        assert_eq!(viewer.commit_filter, older.to_uppercase());
        assert_eq!(matching(viewer.history.as_ref().unwrap()), 1);
        assert!(!viewer.history.as_ref().unwrap().has_more);
    }

    #[test]
    fn external_edits_refresh_even_when_status_does_not_change() {
        let repo = Repo::new();
        repo.write("file", "base\n");
        repo.commit();
        repo.write("file", "one\n");
        let mut request = repo.request(Some(("file".into(), false)));
        let first = refresh(&request).unwrap();
        request.refs = Some(first.refs);
        repo.write("file", "two\n");
        repo.write(".gitignore", "ignored\n");
        repo.write("ignored", "ignored");
        repo.write("added", "new");
        let second = refresh(&request).unwrap();
        assert!(second.history.is_none());
        assert!(
            second
                .selected
                .as_ref()
                .unwrap()
                .1
                .diff
                .rows
                .iter()
                .any(|line| line.text == "two")
        );
        assert!(
            !second
                .comparison
                .files
                .iter()
                .any(|file| file.path == PathBuf::from("ignored"))
        );
        assert!(
            second
                .comparison
                .files
                .iter()
                .any(|file| file.path == PathBuf::from("added"))
        );
        repo.git(&["add", "file"]);
        let staged = refresh(&request).unwrap();
        assert!(staged.comparison.files[staged.selected.unwrap().0].staged);
        repo.git(&["restore", "--staged", "file"]);
        fs::remove_file(repo.0.join("file")).unwrap();
        let deleted = refresh(&request).unwrap();
        assert_eq!(
            deleted.comparison.files[deleted.selected.unwrap().0].status,
            "D"
        );
        repo.git(&["restore", "file"]);
        assert!(refresh(&request).unwrap().selected.is_none());
        repo.git(&["mv", "file", "renamed"]);
        assert!(
            refresh(&request)
                .unwrap()
                .comparison
                .files
                .iter()
                .any(|f| f.path == PathBuf::from("renamed") && f.staged)
        );
    }

    #[test]
    fn refs_refresh_tracks_checkout_but_keeps_explicit_history_and_commits_fixed() {
        let repo = Repo::new();
        let empty = refresh(&repo.request(None)).unwrap();
        assert!(empty.history.unwrap().unwrap().commits.is_empty());
        repo.write("file", "base");
        repo.commit();
        let main = repo.git(&["symbolic-ref", "--short", "HEAD"]);
        let id = repo.git(&["rev-parse", "HEAD"]);
        let mut request = repo.request(None);
        request.refs = Some(refresh(&request).unwrap().refs);
        repo.git(&["checkout", "-qb", "other"]);
        repo.write("file", "next");
        repo.commit();
        let next = refresh(&request).unwrap();
        assert_eq!(next.history.unwrap().unwrap().source, "refs/heads/other");
        request.source = Some(format!("refs/heads/{main}"));
        assert_eq!(
            refresh(&request).unwrap().history.unwrap().unwrap().commits[0].id,
            id
        );
        request.target = git::Target::Commit(id.clone());
        request.comparison = Arc::new(git::compare(&repo.0, request.target.clone()).unwrap());
        request.selected = Some(("file".into(), false));
        assert_eq!(refresh(&request).unwrap().comparison.new, id);
        repo.git(&["branch", "-D", &main]);
        let deleted = refresh(&request).unwrap();
        assert!(deleted.history.unwrap().is_err());
        assert_eq!(deleted.comparison.new, id);
        request.source = None;
        repo.git(&["checkout", "-q", "--detach"]);
        assert!(
            refresh(&request)
                .unwrap()
                .history
                .unwrap()
                .unwrap()
                .branches
                .current
                .is_none()
        );
    }

    #[test]
    fn focus_scheduler_is_single_flight_and_defers_to_staging() {
        let repo = Repo::new();
        let (mut viewer, requests) = viewer(repo.request(None).comparison);
        viewer.selected = None;
        let now = Instant::now();
        viewer.schedule_refresh(false, now);
        assert!(requests.try_recv().is_err());
        viewer.schedule_refresh(true, now);
        assert!(matches!(requests.try_recv(), Ok(Request::Refresh(_))));
        viewer.schedule_refresh(true, now + Duration::from_secs(4));
        assert!(requests.try_recv().is_err());
        viewer.refresh_pending = false;
        viewer.staging_pending = true;
        viewer.schedule_refresh(true, now + Duration::from_secs(5));
        assert!(requests.try_recv().is_err());
        viewer.staging_pending = false;
        viewer.schedule_refresh(false, now + Duration::from_secs(6));
        viewer.next_refresh = now + Duration::from_secs(100);
        viewer.schedule_refresh(true, now + Duration::from_secs(7));
        assert!(matches!(requests.try_recv(), Ok(Request::Refresh(_))));
    }

    #[test]
    fn refresh_preserves_selection_scroll_and_rejects_obsolete_results() {
        let repo = Repo::new();
        repo.write("file", "base");
        repo.commit();
        repo.write("file", "changed");
        let request = repo.request(Some(("file".into(), false)));
        let (mut viewer, requests) = viewer(request.comparison.clone());
        let scroll_key = (file_id(&request.comparison, 0), false, false);
        viewer.scroll_offsets.insert(scroll_key, 75.0);
        repo.write("aaa", "new");
        viewer.accept_refresh(0, request.selected.clone(), Ok(refresh(&request).unwrap()));
        assert_eq!(viewer.selected_key(), request.selected);
        assert_eq!(viewer.selected, Some(1));
        assert_eq!(viewer.pending_scroll, Some((scroll_key, 75.0)));
        let generation = viewer.generation;
        viewer.accept_refresh(0, request.selected.clone(), Err("stale".into()));
        assert!(viewer.refresh_error.is_none());
        viewer.accept_refresh(
            generation,
            Some(("other".into(), false)),
            Err("stale selection".into()),
        );
        assert!(viewer.refresh_error.is_none());
        viewer.accept_refresh(
            generation,
            request.selected.clone(),
            Err("unavailable".into()),
        );
        assert_eq!(viewer.refresh_error.as_deref(), Some("unavailable"));
        assert!(viewer.comparison.is_some());
        viewer.toggle_staging(&[1]);
        assert!(requests.try_recv().is_err());
        viewer.accept_refresh(
            generation,
            request.selected.clone(),
            Ok(refresh(&request).unwrap()),
        );
        assert!(viewer.refresh_error.is_none());
        repo.git(&["add", "file"]);
        let generation = viewer.generation;
        viewer.accept_refresh(
            generation,
            request.selected.clone(),
            Ok(refresh(&request).unwrap()),
        );
        assert_eq!(viewer.selected_key(), Some(("file".into(), true)));
        assert_eq!(viewer.pending_scroll.unwrap().1, 75.0);
    }
}
