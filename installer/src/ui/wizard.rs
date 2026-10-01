// ------------ Install Wizard ------------
// The install window: Welcome, Options (folder, shortcut, Visual C++ runtime, launch afterwards), Progress and Done.
// The chosen folder is checked in the background and the install itself runs on a worker thread.

use std::path::PathBuf;

use eframe::egui::{self, RichText};

use crate::consts;
use crate::install::{human_bytes, launch_app, perform_install, required_bytes, InstallOptions};
use crate::msg::{Cancel, EngineEvent};
use crate::paths;
use crate::payload::Payload;
use crate::ui;

pub fn run(payload: Payload) -> eframe::Result<i32> {
    let options = eframe::NativeOptions {
        viewport: ui::viewport(ui::WINDOW),
        centered: true,
        ..Default::default()
    };
    let outcome = std::rc::Rc::new(std::cell::Cell::new(crate::exit::OK));
    let result_slot = outcome.clone();
    eframe::run_native(
        "Peebify Launcher Setup",
        options,
        Box::new(move |cc| {
            ui::apply_theme(&cc.egui_ctx);
            Ok(Box::new(WizardApp::new(&cc.egui_ctx, payload, result_slot)))
        }),
    )?;
    Ok(outcome.get())
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Welcome,
    Options,
    Progress,
    Done,
}

struct WizardApp {
    payload: Payload,
    logo: Option<egui::TextureHandle>,
    page: Page,
    install_dir: String,
    desktop_shortcut: bool,
    launch_after: bool,
    vc_redist_missing: bool,
    install_vc_redist: bool,
    rx: Option<std::sync::mpsc::Receiver<EngineEvent>>,
    cancel: Cancel,
    cancelling: bool,
    committed: bool,
    pacer: ui::Pacer,
    warnings: Vec<String>,
    error: Option<String>,
    launcher_running: bool,
    closed_launcher: bool,
    running_checked_at: Option<std::time::Instant>,
    page_changed_at: std::time::Instant,
    chrome: ui::Chrome,
    launched_at: Option<std::time::Instant>,
    stay_open: bool,
    webview2_missing: bool,
    registered: Option<PathBuf>,
    target_check: Option<(String, std::time::Instant, TargetCheck)>,
    target_job: Option<(String, std::sync::mpsc::Receiver<TargetCheck>)>,
    target_stale_since: Option<std::time::Instant>,
    outcome: std::rc::Rc<std::cell::Cell<i32>>,
}

struct TargetCheck {
    refusal: Option<&'static str>,
    free: Option<u64>,
    needed: u64,
    replacing: bool,
    foreign: bool,
    elsewhere: Option<PathBuf>,
}

impl TargetCheck {
    fn blocks_install(&self) -> bool {
        self.refusal.is_some() || self.free.is_some_and(|free| free < self.needed)
    }

    fn short_of_space(&self) -> Option<String> {
        match self.free {
            Some(free) if free < self.needed => Some(format!(
                "Needs about {}, only {} free on this drive.",
                human_bytes(self.needed),
                human_bytes(free)
            )),
            _ => None,
        }
    }

    fn size_hint(&self) -> Option<String> {
        if self.refusal.is_some() {
            return None;
        }
        let needed = human_bytes(self.needed);
        let free = self.free.map(human_bytes)?;
        Some(format!("About {needed} needed, {free} free on that drive."))
    }
}

fn compute_target_check(
    current: &str,
    estimated_size_kb: u64,
    registered: Option<&std::path::Path>,
) -> TargetCheck {
    let dir = PathBuf::from(current);
    let refusal = if current.is_empty() {
        Some("Enter a folder to install into.")
    } else {
        paths::validate_install_dir(&dir).err().map(|r| r.message())
    }
    .or_else(|| {
        (!dir.ancestors().any(|a| a.exists()))
            .then_some("That drive isn't available. Pick a folder on a connected drive.")
    })
    .or_else(|| {
        let collides =
            !crate::swap::foreign_collisions(&dir, &crate::swap::expected_top_names())
                .is_empty();
        collides.then_some(
            "This folder already has its own resources or icons, which setup would \
             replace. Choose an empty folder, or add \"Peebify Launcher\" to the end \
             of the path.",
        )
    });
    let usable = refusal.is_none();
    TargetCheck {
        refusal,
        free: if usable {
            crate::win::free_space_bytes(&dir)
        } else {
            None
        },
        needed: required_bytes(estimated_size_kb),
        replacing: usable && dir.join(consts::INSTALL_MANIFEST_NAME).exists(),
        foreign: usable && paths::is_foreign_non_empty(&dir),
        elsewhere: registered
            .filter(|registered| {
                usable && registered.is_dir() && !paths::same_path(registered, &dir)
            })
            .map(std::path::Path::to_path_buf),
    }
}

fn install_allowed(
    check: Option<&(String, std::time::Instant, TargetCheck)>,
    current: &str,
) -> bool {
    check.is_some_and(|(seen, _, check)| seen == current && !check.blocks_install())
}

fn existing_ancestor(path: &str) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    PathBuf::from(path)
        .ancestors()
        .find(|a| !a.as_os_str().is_empty() && a.is_dir())
        .map(std::path::Path::to_path_buf)
}

const TARGET_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);
const TARGET_STALE_GRACE: std::time::Duration = std::time::Duration::from_millis(400);
const CHECKING_FOLDER: &str = "Checking folder…";

fn clean_target(raw: &str) -> String {
    raw.trim().trim_matches('"').trim().to_string()
}

fn initial_install_dir(registered: Option<PathBuf>) -> PathBuf {
    registered
        .filter(|dir| paths::validate_install_dir(dir).is_ok())
        .unwrap_or_else(consts::default_install_dir)
}

fn elsewhere_notice(registered: &std::path::Path) -> String {
    format!(
        "{} is already installed in {}. Installing here leaves that copy in place. \
         Run its {} to remove it.",
        consts::PRODUCT_NAME,
        registered.display(),
        consts::UNINSTALLER_NAME
    )
}

impl WizardApp {
    fn new(
        ctx: &egui::Context,
        payload: Payload,
        outcome: std::rc::Rc<std::cell::Cell<i32>>,
    ) -> Self {
        let vc_redist_missing = !crate::prereqs::vcredist::is_installed();
        let registered = crate::win::registered_install_location();
        Self {
            logo: ui::load_logo(ctx),
            install_dir: initial_install_dir(registered.clone())
                .display()
                .to_string(),
            desktop_shortcut: true,
            launch_after: true,
            vc_redist_missing,
            install_vc_redist: vc_redist_missing,
            page: Page::Welcome,
            rx: None,
            cancel: Cancel::default(),
            cancelling: false,
            committed: false,
            pacer: ui::Pacer::new(""),
            warnings: Vec::new(),
            error: None,
            launcher_running: false,
            closed_launcher: false,
            running_checked_at: None,
            page_changed_at: std::time::Instant::now(),
            chrome: ui::Chrome::default(),
            launched_at: None,
            stay_open: false,
            webview2_missing: false,
            registered,
            target_check: None,
            target_job: None,
            target_stale_since: None,
            outcome,
            payload,
        }
    }

    fn goto(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.page_changed_at = std::time::Instant::now();
        }
    }

    fn enter_pressed(ui_: &egui::Ui) -> bool {
        ui_.input(|i| i.key_pressed(egui::Key::Enter)) && ui_.memory(|m| m.focused().is_none())
    }

    fn launcher_running(&mut self) -> bool {
        let stale = self
            .running_checked_at
            .map(|t| t.elapsed() >= std::time::Duration::from_secs(2))
            .unwrap_or(true);
        if stale {
            self.launcher_running =
                !crate::win::pids_by_exe_name(&self.payload.manifest.main_binary).is_empty();
            self.running_checked_at = Some(std::time::Instant::now());
        }
        self.launcher_running
    }

    fn target_path(&self) -> String {
        clean_target(&self.install_dir)
    }

    fn poll_target_check(&mut self, ctx: &egui::Context) {
        let received = self.target_job.as_ref().map(|(_, rx)| rx.try_recv());
        match received {
            Some(Ok(check)) => {
                if let Some((path, _)) = self.target_job.take() {
                    self.target_check = Some((path, std::time::Instant::now(), check));
                }
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => self.target_job = None,
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => {}
        }

        let current = self.target_path();
        let (fresh, due) = match &self.target_check {
            Some((seen, at, _)) => (
                seen == &current,
                seen != &current || at.elapsed() >= TARGET_RECHECK,
            ),
            None => (false, true),
        };
        if fresh {
            self.target_stale_since = None;
        } else if self.target_stale_since.is_none() {
            self.target_stale_since = Some(std::time::Instant::now());
        }
        if due && self.target_job.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            let path = current.clone();
            let estimated_size_kb = self.payload.manifest.estimated_size_kb;
            let registered = self.registered.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let check = compute_target_check(&path, estimated_size_kb, registered.as_deref());
                let _ = tx.send(check);
                ctx.request_repaint();
            });
            self.target_job = Some((current, rx));
        }
        if !fresh {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn install_ready(&self) -> bool {
        install_allowed(self.target_check.as_ref(), &self.target_path())
    }

    fn shown_check(&self) -> Option<&TargetCheck> {
        let (seen, _, check) = self.target_check.as_ref()?;
        let recent = self
            .target_stale_since
            .is_none_or(|since| since.elapsed() < TARGET_STALE_GRACE);
        (seen == &self.target_path() || recent).then_some(check)
    }

    fn start_install(&mut self) {
        let payload = self.payload.clone();
        let opts = InstallOptions {
            install_dir: PathBuf::from(self.target_path()),
            desktop_shortcut: self.desktop_shortcut,
            install_vc_redist: self.vc_redist_missing && self.install_vc_redist,
        };
        let cancel = self.cancel.clone();
        self.closed_launcher = self.launcher_running();
        self.goto(Page::Progress);
        self.pacer = ui::Pacer::new("Starting…");
        self.rx = Some(ui::spawn_engine(move |sink| {
            perform_install(&payload, &opts, &cancel, sink)
        }));
    }

    fn drain_events(&mut self) {
        let Some(rx) = &self.rx else { return };
        while let Ok(event) = rx.try_recv() {
            match event {
                EngineEvent::Status { phase, percent } => self.pacer.push(phase, percent),
                EngineEvent::Warning(w) => self.warnings.push(w),
                EngineEvent::Committed => {
                    self.committed = true;
                    self.cancelling = false;
                }
                EngineEvent::Finished => {
                    self.webview2_missing = !crate::prereqs::webview2::is_installed();
                    self.pacer.finish();
                    self.rx = None;
                    return;
                }
                EngineEvent::Cancelled => {
                    self.outcome.set(crate::exit::CANCELLED);
                    self.rx = None;
                    self.cancelling = true;
                    return;
                }
                EngineEvent::Failed(e) => {
                    self.outcome.set(crate::exit::FAILED);
                    self.error = Some(e);
                    self.goto(Page::Done);
                    self.rx = None;
                    return;
                }
            }
        }
    }
}

impl eframe::App for WizardApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        ui::clear_color()
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.chrome.apply();
        self.drain_events();
        if matches!(self.page, Page::Welcome | Page::Options) {
            self.poll_target_check(ctx);
        }
        if self.rx.is_some() && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.rx.is_none() && self.outcome.get() == crate::exit::CANCELLED {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if matches!(self.page, Page::Progress) {
            self.pacer.tick();
            if self.pacer.settled() {
                self.goto(Page::Done);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        } else if matches!(self.page, Page::Welcome | Page::Options) {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            match self.page {
                Page::Options => self.goto(Page::Welcome),
                Page::Welcome | Page::Done => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Page::Progress => {}
            }
        }

        let closable = match self.page {
            Page::Progress => ui::Close::Disabled,
            _ => ui::Close::Enabled,
        };
        let logo = self.logo.clone();
        let version = self.payload.manifest.version.clone();
        let failed = self.error.is_some();

        let identity = match self.page {
            Page::Done if failed => format!("Version {version} · not installed"),
            Page::Done => format!("Version {version} · installed"),
            _ => format!("Version {version}"),
        };
        let mut rail = ui::Rail::new(logo.as_ref(), identity);
        if failed && self.page == Page::Done {
            rail = rail.error("ERROR").logo_alpha(0.65);
        } else {
            rail = rail.step(match self.page {
                Page::Welcome | Page::Options => ui::Step::Setup,
                Page::Progress => ui::Step::Install,
                Page::Done => ui::Step::Ready,
            });
        }
        let scrim = match self.page {
            Page::Done if failed => ui::SCRIM_FAILED,
            Page::Done => ui::SCRIM_DONE,
            _ => ui::SCRIM_DEFAULT,
        };

        ui::split(ctx, ui::Surface::Install, scrim, closable, rail, |body| {
            const FADE_SECS: f32 = 0.18;
            let fade = (self.page_changed_at.elapsed().as_secs_f32() / FADE_SECS).clamp(0.0, 1.0);
            if fade < 1.0 {
                body.ctx().request_repaint();
            }
            body.set_opacity(fade);
            match self.page {
                Page::Welcome => self.page_welcome(body),
                Page::Options => self.page_options(body, frame),
                Page::Progress => self.page_progress(body),
                Page::Done => self.page_done(body, ctx),
            }
        });
    }
}

impl WizardApp {
    fn page_welcome(&mut self, ui_: &mut egui::Ui) {
        let launcher_running = self.launcher_running();
        ui_.add_space(16.0);
        ui::heading(ui_, "Setup", 29.0);
        ui_.add_space(14.0);
        ui::paragraph(
            ui_,
            "Let's begin the setup and make the official launchers irrelevant, \
             feel free to fill out your preferences along the way.",
            ui::text::BODY,
            ui::tx(0.68),
            340.0,
        );

        if launcher_running {
            ui_.add_space(16.0);
            ui::bullet_row(
                ui_,
                ui::NOTICE,
                "The launcher's open right now. Don't worry, we'll close it once you \
                 start the installation.",
            );
        }
        if self.vc_redist_missing && self.install_vc_redist {
            ui_.add_space(10.0);
            ui::bullet_row(
                ui_,
                ui::NOTICE,
                "The Microsoft Visual C++ runtime the games need is missing or out of \
                 date. Setup will install it, and Windows will ask for permission.",
            );
        }
        let (replacing, elsewhere, blocker, hint) = match self.shown_check() {
            Some(check) => (
                check.replacing,
                check.elsewhere.as_deref().map(elsewhere_notice),
                check
                    .refusal
                    .map(str::to_string)
                    .or_else(|| check.short_of_space()),
                check.size_hint(),
            ),
            None => (false, None, None, Some(CHECKING_FOLDER.to_string())),
        };
        if replacing {
            ui_.add_space(10.0);
            ui::bullet_row(
                ui_,
                ui::NOTICE,
                "Seems an install already exists! We'll replace only the files needed \
                 for this version.",
            );
        }
        if let Some(elsewhere) = elsewhere {
            ui_.add_space(10.0);
            ui::bullet_row(ui_, ui::NOTICE, &elsewhere);
        }

        let ok = self.install_ready();
        let path = self.target_path();
        let mut start = false;
        let mut customize = false;
        ui::footer(ui_, |foot| {
            ui::action_row(foot, true, |row| {
                start = row
                    .add_enabled_ui(ok, |row| ui::primary_button(row, "Install"))
                    .inner
                    .clicked();
                row.add_space(10.0);
                customize = ui::secondary_button(row, "Customize").clicked();
            });
            foot.add_space(14.0);
            if let Some(blocker) = blocker {
                foot.label(
                    RichText::new(blocker)
                        .size(ui::text::XS)
                        .color(ui::DANGER_TEXT),
                );
                foot.add_space(3.0);
            } else if let Some(hint) = hint {
                foot.label(RichText::new(hint).size(ui::text::XS).color(ui::tx(0.56)));
                foot.add_space(3.0);
            }
            foot.label(RichText::new(path).size(ui::text::MD).color(ui::tx(0.80)));
            foot.add_space(3.0);
            ui::meta_label(foot, "Goes to");
            foot.add_space(14.0);
            ui::hairline(foot);
        });

        if customize {
            self.goto(Page::Options);
        } else if (start || Self::enter_pressed(ui_)) && ok {
            self.start_install();
        }
    }

    fn page_options(&mut self, ui_: &mut egui::Ui, frame: &eframe::Frame) {
        ui_.add_space(8.0);
        ui::heading(ui_, "Preferences", 22.0);
        ui_.add_space(18.0);

        ui_.label(
            RichText::new("Install folder")
                .size(ui::text::SM)
                .color(ui::tx(0.60)),
        );
        ui_.add_space(7.0);
        ui_.horizontal(|row| {
            let width = row.available_width() - 100.0;
            ui::text_field(row, &mut self.install_dir, egui::vec2(width, 36.0));
            row.add_space(8.0);
            if ui::small_button(row, "Browse…").clicked() {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Choose install folder")
                    .set_parent(frame);
                if let Some(start) = existing_ancestor(&clean_target(&self.install_dir)) {
                    dialog = dialog.set_directory(start);
                }
                if let Some(dir) = dialog.pick_folder() {
                    let dir = if dir.file_name().map(|n| n == consts::PRODUCT_NAME) == Some(true) {
                        dir
                    } else {
                        dir.join(consts::PRODUCT_NAME)
                    };
                    self.install_dir = dir.display().to_string();
                }
            }
        });
        ui_.add_space(7.0);
        self.target_feedback(ui_);

        ui_.add_space(20.0);
        ui::checkbox(
            ui_,
            &mut self.desktop_shortcut,
            "Put a shortcut on my desktop",
        );
        ui_.add_space(14.0);
        ui::checkbox(
            ui_,
            &mut self.launch_after,
            "Open the launcher when setup finishes",
        );
        if self.vc_redist_missing {
            ui_.add_space(14.0);
            ui::checkbox(
                ui_,
                &mut self.install_vc_redist,
                "Install or update the Microsoft Visual C++ runtime",
            );
            ui_.add_space(3.0);
            ui::sub_note(
                ui_,
                "Kuro Games, HoYoverse, and Gryphline all use this framework. We can \
                 install it for you if you like.",
            );
        }

        let ok = self.install_ready();
        let mut back = false;
        let mut start = false;
        ui::footer(ui_, |foot| {
            ui::action_row(foot, false, |row| {
                back = ui::secondary_button(row, "Back").clicked();
                row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |row| {
                    start = row
                        .add_enabled_ui(ok, |row| ui::primary_button(row, "Install"))
                        .inner
                        .clicked();
                });
            });
        });

        if back {
            self.goto(Page::Welcome);
        } else if start {
            self.start_install();
        }
    }

    fn target_feedback(&mut self, ui_: &mut egui::Ui) {
        let Some(check) = self.shown_check() else {
            ui_.label(
                RichText::new(CHECKING_FOLDER)
                    .size(ui::text::XS)
                    .color(ui::tx(0.56)),
            );
            return;
        };
        let (refusal, free, needed, replacing, foreign, short, elsewhere) = (
            check.refusal,
            check.free,
            check.needed,
            check.replacing,
            check.foreign,
            check.short_of_space(),
            check.elsewhere.as_deref().map(elsewhere_notice),
        );

        if let Some(refusal) = refusal {
            ui_.label(
                RichText::new(refusal)
                    .size(ui::text::XS)
                    .color(ui::DANGER_TEXT),
            );
            return;
        }

        match (free, short) {
            (_, Some(short)) => {
                ui_.label(RichText::new(short).size(ui::text::XS).color(ui::DANGER_TEXT));
            }
            (Some(free), None) => {
                ui_.label(
                    RichText::new(format!(
                        "Needs about {}, {} free.",
                        human_bytes(needed),
                        human_bytes(free)
                    ))
                    .size(ui::text::XS)
                    .color(ui::tx(0.56)),
                );
            }
            (None, None) => {}
        }

        let notice = if replacing {
            Some(
                "Seems an install already exists! We'll replace only the files needed \
                 for this version.",
            )
        } else if foreign {
            Some(
                "This folder already has files in it. Setup will add to it, and \
                 uninstalling will only remove what it put there.",
            )
        } else {
            None
        };
        if notice.is_some() || elsewhere.is_some() {
            ui_.add_space(10.0);
            ui::card(ui_, ui::Tone::Notice, 10, |card| {
                if let Some(notice) = notice {
                    ui::bullet_row(card, ui::NOTICE, notice);
                }
                if let Some(elsewhere) = &elsewhere {
                    if notice.is_some() {
                        card.add_space(10.0);
                    }
                    ui::bullet_row(card, ui::NOTICE, elsewhere);
                }
            });
        }
    }

    fn page_progress(&mut self, ui_: &mut egui::Ui) {
        ui_.add_space(14.0);
        ui::heading(ui_, "Unpacking", 25.0);
        ui_.add_space(26.0);
        let percent = self.pacer.percent();
        ui_.horizontal(|row| {
            row.label(
                RichText::new(if self.cancelling {
                    "Stopping…"
                } else {
                    self.pacer.phase()
                })
                .size(ui::text::BASE)
                .color(ui::tx(0.85)),
            );
            if percent >= 0.0 {
                row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |row| {
                    row.label(
                        RichText::new(format!("{}%", percent.round() as i32))
                            .size(12.0)
                            .color(ui::tx(0.50)),
                    );
                });
            }
        });
        ui_.add_space(10.0);
        ui::progress_bar(ui_, ui::bar_fraction(percent));
        if self.closed_launcher {
            ui_.add_space(10.0);
            ui_.label(
                RichText::new("The launcher was open, so we went ahead and closed that for you.")
                    .size(ui::text::XS)
                    .color(ui::tx(0.56)),
            );
        }

        let stoppable = !self.cancelling && !self.committed && self.rx.is_some();
        let mut stop = false;
        ui::footer(ui_, |foot| {
            ui::action_row(foot, true, |row| {
                let button = row
                    .add_enabled_ui(stoppable, |row| ui::secondary_button(row, "Cancel"))
                    .inner;
                let button = if self.committed {
                    button.on_disabled_hover_text(
                        "Too late to cancel now, setup is finishing up.",
                    )
                } else {
                    button
                };
                stop = button.clicked();
            });
        });
        if stop {
            self.cancelling = true;
            self.cancel.cancel();
        }
    }

    fn page_done(&mut self, ui_: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(err) = self.error.clone() {
            self.page_failed(ui_, ctx, &err);
            return;
        }
        if !self.webview2_missing {
            self.start_launcher_once();
        }

        if !self.warnings.is_empty() {
            self.page_warnings(ui_, ctx);
            return;
        }

        ui_.add_space(16.0);
        ui::heading(ui_, "All set", 32.0);
        ui_.add_space(14.0);
        ui::paragraph(
            ui_,
            if self.launched_at.is_some() {
                "The launcher is opening now, have fun!"
            } else {
                "Peebify Launcher is installed and ready whenever you are."
            },
            ui::text::BODY,
            ui::tx(0.68),
            330.0,
        );

        let countdown = self.auto_close_remaining();
        if let Some(left) = countdown {
            if left.is_zero() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        let path = self.target_path();
        let label = match countdown {
            Some(left) => format!("Close ({})", left.as_secs() + 1),
            None => "Finish".to_string(),
        };
        let mut close = false;
        let mut open_folder = false;
        ui::footer(ui_, |foot| {
            ui::action_row(foot, true, |row| {
                close = ui::primary_button(row, &label).clicked();
                row.add_space(10.0);
                open_folder = ui::secondary_button(row, "Open folder").clicked();
            });
            foot.add_space(14.0);
            foot.label(RichText::new(&path).size(ui::text::MD).color(ui::tx(0.80)));
            foot.add_space(3.0);
            ui::meta_label(foot, "Installed at");
            foot.add_space(14.0);
            ui::hairline(foot);
        });

        if open_folder {
            self.stay_open = true;
            crate::win::open_folder(&PathBuf::from(&path));
        } else if close || Self::enter_pressed(ui_) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn page_warnings(&mut self, ui_: &mut egui::Ui, ctx: &egui::Context) {
        ui_.add_space(12.0);
        ui::heading(ui_, "Installed, but with errors", 28.0);
        ui_.add_space(20.0);
        let warnings = self.warnings.clone();
        ui::card(ui_, ui::Tone::Notice, 13, |card| {
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .auto_shrink([false, true])
                .show(card, |list| {
                    for (i, warning) in warnings.iter().enumerate() {
                        if i > 0 {
                            list.add_space(10.0);
                        }
                        ui::bullet_row(list, ui::NOTICE, warning);
                    }
                });
        });
        if !self.webview2_missing {
            ui_.add_space(16.0);
            ui::paragraph(
                ui_,
                "Nothing here stops the launcher from running, so feel free to ignore and \
                 gamble away.",
                ui::text::BASE,
                ui::tx(0.62),
                330.0,
            );
        }

        let mut close = false;
        let mut open_folder = false;
        let mut open_log = false;
        ui::footer(ui_, |foot| {
            ui::action_row(foot, true, |row| {
                close = ui::primary_button(row, "Finish").clicked();
                row.add_space(10.0);
                open_folder = ui::secondary_button(row, "Open folder").clicked();
                row.with_layout(egui::Layout::left_to_right(egui::Align::Center), |row| {
                    open_log = ui::link(row, "Open log").clicked();
                });
            });
        });

        if open_log {
            if let Some(dir) = ui::log_dir() {
                crate::win::open_folder(&dir);
            }
        } else if open_folder {
            crate::win::open_folder(&PathBuf::from(self.target_path()));
        } else if close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn start_launcher_once(&mut self) {
        if !self.launch_after || self.launched_at.is_some() {
            return;
        }
        let dir = PathBuf::from(self.target_path());
        match launch_app(&dir, &self.payload.manifest.main_binary) {
            Ok(()) => self.launched_at = Some(std::time::Instant::now()),
            Err(e) => {
                self.launch_after = false;
                self.warnings
                    .push(format!("Peebify Launcher could not be started ({e})."));
            }
        }
    }

    fn auto_close_remaining(&self) -> Option<std::time::Duration> {
        const AUTO_CLOSE: std::time::Duration = std::time::Duration::from_secs(4);
        if !self.warnings.is_empty() || self.stay_open {
            return None;
        }
        let started = self.launched_at?;
        Some(AUTO_CLOSE.saturating_sub(started.elapsed()))
    }

    fn page_failed(&mut self, ui_: &mut egui::Ui, ctx: &egui::Context, err: &str) {
        ui_.add_space(12.0);
        ui::heading(ui_, "Welp that didn't work", 26.0);
        ui_.add_space(12.0);
        let friendly = ui::friendly_error(err);
        ui::paragraph(
            ui_,
            if friendly.is_empty() {
                "Setup couldn't finish. Here's what went wrong:"
            } else {
                friendly
            },
            ui::text::BODY,
            ui::tx(0.72),
            340.0,
        );
        ui_.add_space(18.0);
        ui::detail_box(ui_, ui::Tone::Danger, "Details", err);
        ui_.add_space(10.0);
        let rolled_back = !err.contains(crate::install::ROLLBACK_FAILED);
        ui_.label(
            RichText::new(if rolled_back {
                "Nothing was left half-installed. Setup put everything back the way it \
                 found it."
            } else {
                "Setup could not put everything back. Run setup again to repair the install."
            })
            .size(ui::text::XS)
            .color(ui::tx(0.56)),
        );

        let mut copy = false;
        let mut open_log = false;
        let mut retry = false;
        let mut close = false;
        ui::footer(ui_, |foot| {
            ui::action_row(foot, true, |row| {
                close = ui::primary_button(row, "Close").clicked();
                row.add_space(10.0);
                retry = ui::secondary_button(row, "Try again").clicked();
                row.with_layout(egui::Layout::left_to_right(egui::Align::Center), |row| {
                    copy = ui::secondary_button(row, "Copy details").clicked();
                    row.add_space(12.0);
                    open_log = ui::link(row, "Open log").clicked();
                });
            });
        });

        if copy {
            ctx.copy_text(err.to_string());
        } else if open_log {
            if let Some(dir) = ui::log_dir() {
                crate::win::open_folder(&dir);
            }
        } else if retry {
            self.retry_from_options();
        } else if close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn retry_from_options(&mut self) {
        self.error = None;
        self.outcome.set(crate::exit::OK);
        self.committed = false;
        self.cancelling = false;
        self.cancel = Cancel::default();
        self.warnings.clear();
        self.target_check = None;
        self.target_job = None;
        self.target_stale_since = None;
        self.closed_launcher = false;
        self.goto(Page::Options);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_install_dir_defaults_without_registration() {
        assert_eq!(initial_install_dir(None), consts::default_install_dir());
    }

    #[test]
    fn initial_install_dir_keeps_a_valid_registered_location() {
        let custom = PathBuf::from(r"D:\Apps\Peebify Launcher");
        assert_eq!(initial_install_dir(Some(custom.clone())), custom);
    }

    #[test]
    fn clean_target_strips_copy_as_path_quotes() {
        assert_eq!(
            clean_target(r#"  "D:\Games\Peebify Launcher"  "#),
            r"D:\Games\Peebify Launcher"
        );
        assert_eq!(clean_target(r"D:\Games\Peebify Launcher"), r"D:\Games\Peebify Launcher");
    }

    #[test]
    fn elsewhere_notice_names_the_old_copy_and_its_uninstaller() {
        let notice = elsewhere_notice(&PathBuf::from(r"D:\Apps\Peebify Launcher"));
        assert!(notice.contains(r"D:\Apps\Peebify Launcher"));
        assert!(notice.contains(consts::UNINSTALLER_NAME));
    }

    fn ok_check() -> TargetCheck {
        TargetCheck {
            refusal: None,
            free: Some(10 << 30),
            needed: 1 << 20,
            replacing: false,
            foreign: false,
            elsewhere: None,
        }
    }

    #[test]
    fn install_needs_a_verdict_for_the_current_path() {
        let now = std::time::Instant::now();
        let seen = (r"D:\Games\Peebify Launcher".to_string(), now, ok_check());
        assert!(install_allowed(Some(&seen), r"D:\Games\Peebify Launcher"));
        assert!(!install_allowed(Some(&seen), r"\\nas\share\Peebify Launcher"));
        assert!(!install_allowed(None, r"D:\Games\Peebify Launcher"));
        let refused = (
            seen.0.clone(),
            now,
            TargetCheck {
                refusal: Some("no"),
                ..ok_check()
            },
        );
        assert!(!install_allowed(Some(&refused), &seen.0));
    }

    #[test]
    fn empty_target_is_refused_without_probing() {
        let check = compute_target_check("", 1024, None);
        assert_eq!(check.refusal, Some("Enter a folder to install into."));
        assert!(check.free.is_none() && !check.replacing && !check.foreign);
    }

    #[test]
    fn folder_picker_starts_at_the_deepest_existing_folder() {
        let temp = std::env::temp_dir();
        let typed = temp.join("peebify-no-such-dir").join("Peebify Launcher");
        assert_eq!(existing_ancestor(&typed.display().to_string()), Some(temp));
        assert_eq!(existing_ancestor(""), None);
    }

    #[test]
    fn initial_install_dir_skips_a_refused_registered_location() {
        let refused = PathBuf::from(r"C:\");
        assert_eq!(
            initial_install_dir(Some(refused)),
            consts::default_install_dir()
        );
    }
}
