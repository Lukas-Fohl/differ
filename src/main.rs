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
    sync::{Arc, mpsc},
    thread,
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

enum Event {
    Comparison(Result<Arc<Comparison>, String>),
    File(usize, Result<Arc<Prepared>, String>),
}

enum Request {
    File(usize),
    Reload(std::path::PathBuf),
}

#[derive(Default)]
struct Tree {
    directories: BTreeMap<OsString, Tree>,
    files: Vec<(OsString, usize)>,
}
impl Tree {
    fn new(c: &Comparison) -> Self {
        let mut root = Self::default();
        for (index, file) in c.files.iter().enumerate() {
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
    fn show(&self, ui: &mut egui::Ui, c: &Comparison, selected: &mut Option<usize>) {
        for (name, tree) in &self.directories {
            egui::CollapsingHeader::new(name.to_string_lossy())
                .default_open(true)
                .show(ui, |ui| tree.show(ui, c, selected));
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
            if ui
                .selectable_label(*selected == Some(*index), label)
                .on_hover_text(file.path.to_string_lossy())
                .clicked()
            {
                *selected = Some(*index);
            }
        }
    }
}

struct Viewer {
    events: mpsc::Receiver<Event>,
    requests: mpsc::Sender<Request>,
    repo_input: String,
    browser_open: bool,
    browser_path: std::path::PathBuf,
    comparison: Option<Arc<Comparison>>,
    tree: Tree,
    error: Option<String>,
    selected: Option<usize>,
    cache: HashMap<usize, Result<Arc<Prepared>, String>>,
    requested: std::collections::HashSet<usize>,
    split: bool,
    full: bool,
    light_mode: bool,
    code_zoom: f32,
    scroll_offsets: HashMap<(usize, bool, bool), f32>,
    pending_scroll: Option<((usize, bool, bool), f32)>,
}
impl Viewer {
    fn start(options: cli::Options) -> Self {
        let (events_tx, events) = mpsc::channel();
        let (requests, requests_rx) = mpsc::channel();
        let repo_input = project_path_for_input(&options.repo);
        let browser_path = home_directory();
        thread::spawn(move || {
            let repo = options.repo;
            let target = options.target;
            let comparison = git::compare(&repo, target.clone()).map(Arc::new);
            let _ = events_tx.send(Event::Comparison(comparison.clone()));
            let mut comparison = comparison.ok();
            while let Ok(request) = requests_rx.recv() {
                match request {
                    Request::Reload(new_repo) => {
                        let result = git::compare(&new_repo, target.clone()).map(Arc::new);
                        comparison = result.as_ref().ok().cloned();
                        if events_tx.send(Event::Comparison(result)).is_err() {
                            break;
                        }
                    }
                    Request::File(index) => {
                        let result = comparison
                            .as_ref()
                            .ok_or_else(|| "Could not open comparison".to_owned())
                            .and_then(|comparison| {
                                git::load_file(comparison, index).map_err(|e| e.to_string())
                            })
                            .map(Prepared::new)
                            .map(Arc::new);
                        if events_tx.send(Event::File(index, result)).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self {
            events,
            requests,
            repo_input,
            browser_open: false,
            browser_path,
            comparison: None,
            tree: Tree::default(),
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
    fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Comparison(Ok(c)) => {
                    self.tree = Tree::new(&c);
                    self.selected = (!c.files.is_empty()).then_some(0);
                    self.comparison = Some(c);
                    self.cache.clear();
                    self.requested.clear();
                    self.error = None;
                }
                Event::Comparison(Err(error)) => {
                    self.cache.clear();
                    self.requested.clear();
                    self.error = Some(error);
                }
                Event::File(index, result) => {
                    self.cache.insert(index, result);
                }
            }
        }
        if let Some(index) = self.selected {
            if self.requested.insert(index) {
                let _ = self.requests.send(Request::File(index));
            }
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

fn draw(mut contexts: EguiContexts, mut viewer: NonSendMut<Viewer>) -> Result {
    let ctx = contexts.ctx_mut()?;
    viewer.poll();
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
        let scroll_key = viewer
            .selected
            .map(|index| (index, viewer.split, viewer.full));
        adjust_code_zoom(&mut viewer, zoom_in, scroll_key);
    }
    ctx.set_visuals(if viewer.light_mode {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    });
    if (viewer.comparison.is_none() && viewer.error.is_none())
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
                .map(|index| (index, viewer.split, viewer.full));
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
                viewer.cache.clear();
                viewer.requested.clear();
                viewer.error = None;
                let repo = viewer
                    .comparison
                    .as_ref()
                    .map(|comparison| comparison.repo.clone())
                    .unwrap_or_else(|| std::path::PathBuf::from(&viewer.repo_input));
                let _ = viewer.requests.send(Request::Reload(repo));
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
            if ui.button("Browse…").clicked() {
                viewer.browser_open = true;
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
            if !viewer.repo_input.trim().is_empty() && (open_clicked || enter_pressed) {
                let repo = project_path_from_input(std::path::Path::new(viewer.repo_input.trim()));
                viewer.cache.clear();
                viewer.requested.clear();
                viewer.comparison = None;
                viewer.tree = Tree::default();
                viewer.selected = None;
                viewer.error = None;
                let _ = viewer.requests.send(Request::Reload(repo));
            }
        });
        if let Some(c) = viewer.comparison.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(c.repo.to_string_lossy()).weak());
                ui.separator();
                ui.label(&c.label);
            });
        }
        ui.add_space(4.0);
    });
    if viewer.browser_open {
        let mut browser_open = viewer.browser_open;
        let mut navigate_to = None;
        let mut selected_folder = None;
        egui::Window::new("Select project folder")
            .open(&mut browser_open)
            .resizable(true)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("↑ Parent").clicked() {
                        navigate_to = viewer
                            .browser_path
                            .parent()
                            .map(std::path::Path::to_path_buf);
                    }
                    ui.label(viewer.browser_path.to_string_lossy());
                });
                ui.separator();
                let mut directories = match std::fs::read_dir(&viewer.browser_path) {
                    Ok(entries) => entries
                        .filter_map(Result::ok)
                        .filter_map(|entry| {
                            entry
                                .file_type()
                                .ok()
                                .filter(|kind| kind.is_dir())
                                .map(|_| (entry.file_name(), entry.path()))
                        })
                        .collect::<Vec<_>>(),
                    Err(error) => {
                        ui.colored_label(RED, format!("Could not read folder: {error}"));
                        Vec::new()
                    }
                };
                directories.sort_by(|a, b| a.0.cmp(&b.0));
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for (name, path) in directories {
                            if ui
                                .button(format!("📁 {}", name.to_string_lossy()))
                                .clicked()
                            {
                                navigate_to = Some(path);
                            }
                        }
                    });
                ui.separator();
                if ui.button("Use this folder").clicked() {
                    selected_folder = Some(viewer.browser_path.clone());
                }
            });
        viewer.browser_open = browser_open;
        if let Some(path) = navigate_to {
            viewer.browser_path = path;
        }
        if let Some(path) = selected_folder {
            viewer.repo_input = project_path_for_input(&path);
            viewer.browser_open = false;
        }
    }
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
                egui::ScrollArea::both().show(ui, |ui| viewer.tree.show(ui, &c, &mut selected));
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
            ui.heading("No changes");
            ui.label(if c.working_tree {
                "No unstaged tracked-file changes."
            } else {
                "These revisions have identical file trees."
            });
            return;
        };
        let file = &c.files[index];
        ui.heading(file.path.to_string_lossy());
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
        let viewport_width = ui.available_width();
        let zoom = viewer.code_zoom;
        let row_height = CODE_ROW_HEIGHT * zoom;
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
                    .id_salt((index, "code-horizontal"))
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
        let full = viewer.full;
        let scroll_key = (index, split, full);
        let max_scroll = (rows.len() as f32 * row_height - ui.available_height()).max(0.0);
        let restore_offset = viewer
            .pending_scroll
            .filter(|(key, _)| *key == scroll_key)
            .map(|(_, offset)| clamp_scroll_offset(offset, max_scroll));
        if restore_offset.is_some() {
            viewer.pending_scroll = None;
        }
        let scroll = scroll.id_salt(scroll_key).auto_shrink([false, false]);
        let scroll = if let Some(offset) = restore_offset {
            scroll.vertical_scroll_offset(offset)
        } else {
            scroll
        };
        let output = scroll.show_rows(ui, row_height, rows.len(), |ui, range| {
            ui.set_min_width(if split { width * 2.0 + 12.0 } else { width });
            for row_index in range {
                let row = &rows[row_index];
                ui.push_id(row_index, |ui| {
                    if let Some(count) = row.gap {
                        ui.add_sized(
                            [width, row_height],
                            egui::Label::new(
                                egui::RichText::new(format!("··· {count} unchanged lines ···"))
                                    .weak(),
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
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(width, row_height),
                            egui::Sense::hover(),
                        );
                        cell(ui, &prepared.diff, row.left, false, true, rect, 0.0, zoom);
                    }
                });
            }
        });
        viewer
            .scroll_offsets
            .insert(scroll_key, output.state.offset.y);
    });
    viewer.pending_scroll = None;
    Ok(())
}

fn adjust_code_zoom(viewer: &mut Viewer, zoom_in: bool, scroll_key: Option<(usize, bool, bool)>) {
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
