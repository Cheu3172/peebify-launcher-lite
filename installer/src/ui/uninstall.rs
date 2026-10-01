// ------------ Uninstall Window ------------
// What opens from Installed apps or uninstall.exe. Shows the version and size, lets you also delete settings and
// installed games (Steam and protected folders are kept), then hands off to the second stage that does the removal.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};

use crate::install::{
    dir_size, game_dir_verdict, human_bytes, installed_game_dirs, respawn_for_uninstall,
    GameDirVerdict, InstallManifest, UninstallOptions,
};
use crate::ui;

fn identity_line(install_dir: &Path) -> String {
    let Ok(manifest) = InstallManifest::load(install_dir) else {
        return "Installed on this PC".to_string();
    };
    let mut parts = vec![manifest.version.clone()];
    if let Ok(when) = chrono::DateTime::parse_from_rfc3339(&manifest.installed_at) {
        use chrono::Datelike;
        let local = when.with_timezone(&chrono::Local);
        parts.push(format!(
            "installed {} {}, {}",
            local.format("%B"),
            local.day(),
            local.year()
        ));
    }
    let footprint: u64 = manifest
        .files
        .iter()
        .filter_map(|f| std::fs::metadata(install_dir.join(f)).ok())
        .map(|m| m.len())
        .sum();
    if footprint > 0 {
        parts.push(human_bytes(footprint));
    }
    parts.join(" · ")
}

#[derive(Default)]
struct Games {
    dirs: Option<Vec<(PathBuf, GameDirVerdict)>>,
    sizes: Vec<Option<u64>>,
}

fn scan_games(install_dir: PathBuf, games: Arc<Mutex<Games>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let found: Vec<(PathBuf, GameDirVerdict)> = installed_game_dirs(&install_dir)
            .into_iter()
            .map(|dir| {
                let verdict = game_dir_verdict(&dir);
                (dir, verdict)
            })
            .collect();
        let dirs: Vec<PathBuf> = found.iter().map(|(dir, _)| dir.clone()).collect();
        if let Ok(mut games) = games.lock() {
            games.sizes = vec![None; found.len()];
            games.dirs = Some(found);
        }
        ctx.request_repaint();
        for (i, dir) in dirs.iter().enumerate() {
            let size = dir_size(dir);
            if let Ok(mut games) = games.lock() {
                games.sizes[i] = Some(size);
            }
            ctx.request_repaint();
        }
    });
}

pub fn run(install_dir: PathBuf) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: ui::viewport(ui::WINDOW),
        centered: true,
        ..Default::default()
    };
    let identity = identity_line(&install_dir);
    eframe::run_native(
        "Uninstall Peebify Launcher",
        options,
        Box::new(move |cc| {
            ui::apply_theme(&cc.egui_ctx);
            let games = Arc::new(Mutex::new(Games::default()));
            scan_games(install_dir.clone(), games.clone(), cc.egui_ctx.clone());
            Ok(Box::new(UninstallApp {
                logo: ui::load_logo(&cc.egui_ctx),
                identity,
                install_dir,
                games,
                remove_data: false,
                remove_games: false,
                error: None,
                chrome: ui::Chrome::default(),
            }))
        }),
    )
}

struct UninstallApp {
    logo: Option<egui::TextureHandle>,
    identity: String,
    install_dir: PathBuf,
    games: Arc<Mutex<Games>>,
    remove_data: bool,
    remove_games: bool,
    error: Option<String>,
    chrome: ui::Chrome,
}

impl UninstallApp {
    fn still_measuring(&self) -> bool {
        self.games
            .lock()
            .map(|g| g.dirs.is_none() || g.sizes.iter().any(Option::is_none))
            .unwrap_or(false)
    }

    fn any_removable(&self) -> Option<bool> {
        let games = self.games.lock().ok()?;
        let dirs = games.dirs.as_ref()?;
        Some(
            dirs.iter()
                .any(|(_, verdict)| *verdict == GameDirVerdict::Removable),
        )
    }
}

impl eframe::App for UninstallApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        ui::clear_color()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.chrome.apply();
        if self.still_measuring() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let logo = self.logo.clone();
        let rail = ui::Rail::new(logo.as_ref(), self.identity.clone()).logo_alpha(0.75);

        ui::split(
            ctx,
            ui::Surface::Uninstall,
            ui::SCRIM_UNINSTALL,
            ui::Close::Enabled,
            rail,
            |body| {
                body.add_space(12.0);
                ui::heading(body, "UNINSTALL", 26.0);
                body.add_space(12.0);
                ui::paragraph(
                    body,
                    "This removes the launcher itself, but you can choose what you do \
                     and don't want removed.",
                    ui::text::BODY,
                    ui::tx(0.68),
                    340.0,
                );

                body.add_space(20.0);
                ui::card(body, ui::Tone::Plain, 15, |card| {
                    ui::checkbox(
                        card,
                        &mut self.remove_data,
                        "Also delete settings, mods and launcher data",
                    );
                    card.add_space(3.0);
                    ui::sub_note(
                        card,
                        "Mods in the default mods folder and the mod loader are removed \
                         too. Installed games are not touched by this option.",
                    );
                    match self.any_removable() {
                        Some(true) => {
                            card.add_space(13.0);
                            ui::checkbox(
                                card,
                                &mut self.remove_games,
                                "Also delete installed game files",
                            );
                            card.add_space(13.0);
                            self.game_list(card);
                        }
                        Some(false) => {}
                        None => {
                            card.add_space(13.0);
                            card.label(
                                RichText::new("Checking installed games…")
                                    .size(ui::text::SM)
                                    .color(ui::tx(0.56)),
                            );
                        }
                    }
                });

                if let Some(err) = &self.error {
                    body.add_space(10.0);
                    body.label(
                        RichText::new(err.as_str())
                            .size(ui::text::XS)
                            .color(ui::DANGER_TEXT),
                    );
                }

                let mut remove = false;
                let mut keep = false;
                ui::footer(body, |foot| {
                    ui::action_row(foot, true, |row| {
                        let label = if self.remove_games {
                            "Uninstall and delete games"
                        } else {
                            "Uninstall"
                        };
                        remove = ui::danger_button(row, label).clicked();
                        row.add_space(10.0);
                        keep = ui::secondary_button(row, "Keep it").clicked();
                    });
                });

                if keep {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else if remove {
                    let opts = UninstallOptions {
                        install_dir: self.install_dir.clone(),
                        remove_data: self.remove_data,
                        remove_games: self.remove_games,
                    };
                    match respawn_for_uninstall(&opts, false) {
                        Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(e) => self.error = Some(e),
                    }
                }
            },
        );
    }
}

impl UninstallApp {
    fn game_list(&mut self, ui_: &mut egui::Ui) {
        let (dirs, sizes) = self
            .games
            .lock()
            .ok()
            .map(|g| (g.dirs.clone().unwrap_or_default(), g.sizes.clone()))
            .unwrap_or_default();
        let color = if self.remove_games {
            ui::DANGER_TEXT
        } else {
            ui::tx(0.62)
        };
        egui::ScrollArea::vertical()
            .max_height(84.0)
            .auto_shrink([false, true])
            .show(ui_, |list| {
                list.spacing_mut().item_spacing.y = 6.0;
                for (i, (dir, verdict)) in dirs.iter().enumerate() {
                    let path = dir.display().to_string();
                    let size = match sizes.get(i).copied().flatten() {
                        Some(bytes) => human_bytes(bytes),
                        None => "measuring…".to_string(),
                    };
                    let kept = match verdict {
                        GameDirVerdict::Removable => None,
                        GameDirVerdict::Steam => Some("Steam, kept"),
                        GameDirVerdict::Guarded => Some("kept"),
                    };
                    let path_color = if kept.is_some() { ui::tx(0.56) } else { color };
                    list.horizontal(|row| {
                        row.add_space(28.0);
                        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |row| {
                            row.label(RichText::new(size).size(ui::text::XS).color(ui::tx(0.56)));
                            if let Some(tag) = kept {
                                row.add_space(12.0);
                                row.label(
                                    RichText::new(tag).size(ui::text::XS).color(ui::tx(0.62)),
                                );
                            }
                            row.add_space(12.0);
                            row.add(
                                egui::Label::new(
                                    RichText::new(&path).size(ui::text::XS).color(path_color),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&path);
                        });
                    });
                }
            });
    }
}
