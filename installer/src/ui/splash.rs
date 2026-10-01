// ------------ Progress Splash ------------
// A small progress window for work that needs no wizard. Today that is the uninstall second stage, which shows the
// phase, a progress bar and a done, partial or failed message.

use eframe::egui::{self, RichText};

use crate::ilog::ilog;
use crate::msg::{self, EngineEvent, EngineWork};
use crate::ui;

pub struct SplashSpec {
    pub window_title: String,
    pub title: String,
    pub hint: String,
    pub show_logo: bool,
    pub surface: ui::Surface,
    pub done: Option<SplashDone>,
    pub failed_heading: String,
}

pub struct SplashDone {
    pub success: (String, String),
    pub partial: (String, String),
}

impl SplashSpec {
    pub fn uninstall() -> Self {
        Self {
            window_title: "Uninstall Peebify Launcher".into(),
            title: "Removing Peebify Launcher".into(),
            hint: "This only takes a moment.".into(),
            show_logo: false,
            surface: ui::Surface::Uninstall,
            done: Some(SplashDone {
                success: (
                    "All done.".into(),
                    "Peebify Launcher has been removed. We hope you return to us soon!".into(),
                ),
                partial: (
                    "Uninstall finished".into(),
                    "Some items could not be removed. Open the log for details.".into(),
                ),
            }),
            failed_heading: "Uninstall failed".into(),
        }
    }
}

pub fn run(spec: SplashSpec, work: EngineWork) -> eframe::Result<i32> {
    let options = eframe::NativeOptions {
        viewport: ui::viewport(ui::SPLASH),
        centered: true,
        ..Default::default()
    };
    let window_title = spec.window_title.clone();
    let outcome = std::rc::Rc::new(std::cell::Cell::new(crate::exit::OK));
    let result_slot = outcome.clone();
    let pending = std::rc::Rc::new(std::cell::RefCell::new(Some(work)));
    let work_slot = pending.clone();

    let result = eframe::run_native(
        &window_title,
        options,
        Box::new(move |cc| {
            ui::apply_theme(&cc.egui_ctx);
            let rx = work_slot.borrow_mut().take().map(|work| ui::spawn_engine(work));
            Ok(Box::new(SplashApp {
                logo: ui::load_logo(&cc.egui_ctx),
                spec,
                rx,
                pacer: ui::Pacer::new("Preparing…"),
                error: None,
                warnings: Vec::new(),
                chrome: ui::Chrome::default(),
                outcome: result_slot,
            }))
        }),
    );
    if let Err(e) = result {
        let Some(work) = pending.borrow_mut().take() else {
            return Err(e);
        };
        ilog!("{window_title}: the window could not open ({e}), running without it");
        return Ok(crate::engine_exit_code(
            work(&msg::log_sink()),
            &window_title,
        ));
    }
    Ok(outcome.get())
}

fn warning_detail(warnings: &[String]) -> Option<String> {
    let first = warnings.first()?;
    Some(match warnings.len() {
        1 => first.clone(),
        n => format!("{first} ({} more in the log)", n - 1),
    })
}

struct SplashApp {
    logo: Option<egui::TextureHandle>,
    spec: SplashSpec,
    rx: Option<std::sync::mpsc::Receiver<EngineEvent>>,
    pacer: ui::Pacer,
    error: Option<String>,
    warnings: Vec<String>,
    chrome: ui::Chrome,
    outcome: std::rc::Rc<std::cell::Cell<i32>>,
}

impl eframe::App for SplashApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        ui::clear_color()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.chrome.apply();
        let mut terminal = false;
        let mut cancelled = false;
        if let Some(rx) = &self.rx {
            while let Ok(event) = rx.try_recv() {
                match event {
                    EngineEvent::Status { phase, percent } => self.pacer.push(phase, percent),
                    EngineEvent::Warning(w) => self.warnings.push(w),
                    EngineEvent::Committed => {}
                    EngineEvent::Finished => {
                        self.pacer.finish();
                        terminal = true;
                    }
                    EngineEvent::Cancelled => {
                        self.outcome.set(crate::exit::CANCELLED);
                        cancelled = true;
                        terminal = true;
                    }
                    EngineEvent::Failed(e) => {
                        self.error = Some(e);
                        self.outcome.set(crate::exit::FAILED);
                        terminal = true;
                    }
                }
                if terminal {
                    break;
                }
            }
        }
        if terminal {
            self.rx = None;
        }
        if self.rx.is_some() && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.pacer.tick();
        let finished = self.pacer.settled();
        if (finished && self.spec.done.is_none()) || cancelled {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let working = self.error.is_none() && !finished;
        if working {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }

        let logo = self.logo.clone();
        let closable = if working {
            ui::Close::Disabled
        } else {
            ui::Close::Enabled
        };

        ui::compact(ctx, self.spec.surface, closable, |body| {
            if let Some(err) = self.error.clone() {
                self.failure_panel(body, ctx, &err);
                return;
            }

            if finished {
                let detail = warning_detail(&self.warnings);
                let (heading, message) = match &self.spec.done {
                    Some(done) if detail.is_some() => done.partial.clone(),
                    Some(done) => done.success.clone(),
                    None => ("Done".into(), String::new()),
                };
                body.label(ui::semibold(heading, 17.0));
                body.add_space(6.0);
                ui::paragraph(body, &message, ui::text::MD, ui::tx(0.65), 400.0);
                if let Some(detail) = &detail {
                    body.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .max_height((body.available_height() - 50.0).max(20.0))
                        .auto_shrink([false, true])
                        .show(body, |text| {
                            ui::selectable_paragraph(
                                text,
                                detail,
                                ui::text::XS,
                                ui::tx(0.56),
                                420.0,
                            );
                        });
                }
                ui::footer(body, |foot| {
                    ui::action_row(foot, false, |row| {
                        if detail.is_some() && ui::secondary_button(row, "Open log").clicked() {
                            if let Some(dir) = ui::log_dir() {
                                crate::win::open_folder(&dir);
                            }
                        }
                        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |row| {
                            if ui::primary_button(row, "Close").clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                    });
                });
                return;
            }

            body.horizontal_top(|row| {
                if self.spec.show_logo {
                    if let Some(logo) = &logo {
                        let (rect, _) =
                            row.allocate_exact_size(egui::vec2(38.0, 38.0), egui::Sense::hover());
                        egui::Image::new(logo)
                            .corner_radius(egui::CornerRadius::same(10))
                            .paint_at(row, rect);
                        row.add_space(14.0);
                    }
                }
                row.vertical(|col| {
                    col.spacing_mut().item_spacing.y = 0.0;
                    col.label(ui::semibold(self.spec.title.as_str(), 16.0));
                    col.add_space(3.0);
                    col.label(
                        RichText::new(self.pacer.phase())
                            .size(ui::text::SM)
                            .color(ui::tx(0.50)),
                    );
                });
            });

            let percent = self.pacer.percent();
            let hint = self.spec.hint.clone();
            ui::footer(body, |foot| {
                foot.horizontal(|row| {
                    row.label(RichText::new(hint).size(ui::text::XS).color(ui::tx(0.56)));
                    if percent >= 0.0 {
                        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |row| {
                            row.label(
                                RichText::new(format!("{}%", percent.round() as i32))
                                    .size(ui::text::XS)
                                    .color(ui::tx(0.50)),
                            );
                        });
                    }
                });
                foot.add_space(9.0);
                ui::progress_bar(foot, ui::bar_fraction(percent));
            });
        });
    }
}

impl SplashApp {
    fn failure_panel(&mut self, body: &mut egui::Ui, ctx: &egui::Context, err: &str) {
        body.label(ui::semibold(self.spec.failed_heading.as_str(), 16.0));
        body.add_space(6.0);

        let friendly = ui::friendly_error(err);
        egui::ScrollArea::vertical()
            .max_height((body.available_height() - 50.0).max(20.0))
            .auto_shrink([false, true])
            .show(body, |text| {
                if friendly.is_empty() {
                    ui::selectable_paragraph(text, err, ui::text::SM, ui::tx(0.72), 420.0);
                } else {
                    ui::paragraph(text, friendly, ui::text::SM, ui::tx(0.72), 420.0);
                    text.add_space(4.0);
                    ui::selectable_paragraph(text, err, ui::text::XS, ui::tx(0.56), 420.0);
                }
            });

        ui::footer(body, |foot| {
            ui::action_row(foot, false, |row| {
                if ui::secondary_button(row, "Copy details").clicked() {
                    ctx.copy_text(err.to_string());
                }
                row.add_space(10.0);
                if ui::secondary_button(row, "Open log").clicked() {
                    if let Some(dir) = ui::log_dir() {
                        crate::win::open_folder(&dir);
                    }
                }
                row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |row| {
                    if ui::primary_button(row, "Close").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::warning_detail;

    #[test]
    fn no_warnings_means_no_detail() {
        assert_eq!(warning_detail(&[]), None);
    }

    #[test]
    fn single_warning_is_shown_as_is() {
        let warnings = vec!["Could not remove D:/Games/Genshin: in use".to_string()];
        assert_eq!(warning_detail(&warnings).as_deref(), Some(warnings[0].as_str()));
    }

    #[test]
    fn extra_warnings_are_counted() {
        let warnings = vec!["first".to_string(), "second".to_string(), "third".to_string()];
        assert_eq!(
            warning_detail(&warnings).as_deref(),
            Some("first (2 more in the log)")
        );
    }
}
