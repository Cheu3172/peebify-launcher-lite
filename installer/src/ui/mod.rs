// ------------ Setup Window Kit ------------
// The shared look of the setup windows, drawn with egui: Peebify colors and fonts, wallpaper backdrop, titlebar,
// step rail, buttons, cards and the progress bar. The install wizard, uninstall window and splash all use it.

pub mod splash;
pub mod uninstall;
pub mod wizard;

use eframe::egui::{self, Color32, CornerRadius, RichText};

pub const LOGO_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../webui/public/icons/app.png"
));

const WALLPAPER_JPG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wall.jpg"));
const WALLPAPER_BLUR_JPG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wall-blur.jpg"));

pub mod text {
    pub const MICRO: f32 = 10.5;
    pub const XS: f32 = 11.0;
    pub const SM: f32 = 11.5;
    pub const MD: f32 = 12.5;
    pub const BASE: f32 = 13.0;
    pub const BODY: f32 = 13.5;
}

pub const WINDOW: [f32; 2] = [820.0, 540.0];
pub const SPLASH: [f32; 2] = [520.0, 232.0];

const RAIL_W: f32 = 312.0;
const RAIL_PAD_X: f32 = 26.0;
const RAIL_PAD_TOP: f32 = 30.0;
const RAIL_PAD_BOTTOM: f32 = 28.0;
const PANEL_PAD_X: f32 = 38.0;
const PANEL_PAD_TOP: f32 = 34.0;
const PANEL_PAD_BOTTOM: f32 = 30.0;
const SPLASH_PAD_X: f32 = 28.0;
const SPLASH_PAD_TOP: f32 = 28.0;
const SPLASH_PAD_BOTTOM: f32 = 26.0;

pub const BG: Color32 = Color32::from_rgb(0x0b, 0x0c, 0x14);
pub const TEXT: Color32 = Color32::from_rgb(0xe9, 0xe9, 0xed);
pub const NOTICE: Color32 = Color32::from_rgb(0xe8, 0xb4, 0x4c);
pub const DANGER: Color32 = Color32::from_rgb(0xe0, 0x65, 0x5f);
pub const DANGER_TEXT: Color32 = Color32::from_rgb(0xff, 0x9b, 0x95);
const PANEL_TINT: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x18);

const SEMIBOLD: &str = "peebify-semibold";

pub fn tx(alpha: f32) -> Color32 {
    TEXT.gamma_multiply(alpha)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Surface {
    Install,
    Uninstall,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Close {
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Step {
    Setup,
    Install,
    Ready,
}

#[derive(Clone, Copy)]
pub struct Scrim {
    pub top: f32,
    pub bottom: f32,
}

impl Scrim {
    pub const fn new(top: f32, bottom: f32) -> Self {
        Self { top, bottom }
    }
}

pub const SCRIM_DEFAULT: Scrim = Scrim::new(0.50, 0.72);
pub const SCRIM_DONE: Scrim = Scrim::new(0.44, 0.70);
pub const SCRIM_FAILED: Scrim = Scrim::new(0.62, 0.80);
pub const SCRIM_UNINSTALL: Scrim = Scrim::new(0.56, 0.78);
pub const SCRIM_SPLASH: Scrim = Scrim::new(0.60, 0.88);

pub struct Rail<'a> {
    pub step: Option<Step>,
    pub error: Option<&'a str>,
    pub logo: Option<&'a egui::TextureHandle>,
    pub logo_alpha: f32,
    pub identity: String,
}

impl<'a> Rail<'a> {
    pub fn new(logo: Option<&'a egui::TextureHandle>, identity: impl Into<String>) -> Self {
        Self {
            step: None,
            error: None,
            logo,
            logo_alpha: 1.0,
            identity: identity.into(),
        }
    }

    pub fn step(mut self, step: Step) -> Self {
        self.step = Some(step);
        self
    }

    pub fn error(mut self, label: &'a str) -> Self {
        self.error = Some(label);
        self
    }

    pub fn logo_alpha(mut self, alpha: f32) -> Self {
        self.logo_alpha = alpha;
        self
    }
}

// ------------ Window And Theme ------------
// Creating the borderless window, its icon, fonts and the dark theme.
pub fn viewport(size: [f32; 2]) -> egui::ViewportBuilder {
    let mut builder = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_decorations(false)
        .with_transparent(false)
        .with_resizable(false);
    if let Some(icon) = load_icon() {
        builder = builder.with_icon(std::sync::Arc::new(icon));
    }
    builder
}

pub fn clear_color() -> [f32; 4] {
    let [r, g, b, _] = BG.to_normalized_gamma_f32();
    [r, g, b, 1.0]
}

#[derive(Default)]
pub struct Chrome {
    applied: bool,
}

impl Chrome {
    pub fn apply(&mut self) {
        if !self.applied {
            self.applied = crate::win::round_own_windows();
        }
    }
}

fn logo_rgba() -> &'static (Vec<u8>, u32, u32) {
    static LOGO: std::sync::OnceLock<(Vec<u8>, u32, u32)> = std::sync::OnceLock::new();
    LOGO.get_or_init(|| match image::load_from_memory(LOGO_PNG) {
        Ok(img) => {
            let img = img.into_rgba8();
            let (w, h) = img.dimensions();
            (img.into_raw(), w, h)
        }
        Err(_) => (Vec::new(), 0, 0),
    })
}

fn load_icon() -> Option<egui::IconData> {
    let (rgba, width, height) = logo_rgba();
    if rgba.is_empty() {
        return None;
    }
    Some(egui::IconData {
        rgba: rgba.clone(),
        width: *width,
        height: *height,
    })
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let font_dir = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let mut load = |name: &str, file: &str| -> bool {
        let path = font_dir.join(file);
        match std::fs::read(&path) {
            Ok(bytes) => {
                fonts
                    .font_data
                    .insert(name.to_owned(), egui::FontData::from_owned(bytes).into());
                true
            }
            Err(_) => false,
        }
    };

    let regular = load("peebify-ui", "segoeui.ttf");
    let semibold = load(SEMIBOLD, "seguisb.ttf");

    if regular {
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            list.insert(0, "peebify-ui".to_owned());
        }
    }
    let mut chain = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if semibold {
        chain.insert(0, SEMIBOLD.to_owned());
    }
    fonts
        .families
        .insert(egui::FontFamily::Name(SEMIBOLD.into()), chain);

    ctx.set_fonts(fonts);
}

pub fn semibold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text)
        .size(size)
        .family(egui::FontFamily::Name(SEMIBOLD.into()))
        .color(Color32::WHITE)
}

pub fn heading(ui: &mut egui::Ui, text: &str, size: f32) {
    ui.label(semibold(text, size));
}

pub fn apply_theme(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut style = (*ctx.style()).clone();
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.window_fill = BG;
    visuals.selection.bg_fill = Color32::from_white_alpha(52);
    visuals.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(4);
        w.bg_fill = Color32::TRANSPARENT;
        w.weak_bg_fill = Color32::TRANSPARENT;
        w.bg_stroke = egui::Stroke::new(1.0, tx(0.16));
    }
    visuals.widgets.noninteractive.weak_bg_fill = PANEL_TINT;
    visuals.extreme_bg_color = tx(0.05);
    style.visuals = visuals;
    style.interaction.selectable_labels = false;
    style.spacing.button_padding = egui::vec2(16.0, 8.0);
    style.spacing.item_spacing.y = 6.0;
    ctx.set_style(style);
}

// ------------ Backdrop And Titlebar ------------
// Loads the logo and wallpaper and paints the frosted backdrop, the drag strip and the minimize and close buttons.
pub fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let (rgba, width, height) = logo_rgba();
    if rgba.is_empty() {
        return None;
    }
    let size = [*width as usize, *height as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba);
    Some(ctx.load_texture("peebify-logo", color, egui::TextureOptions::LINEAR))
}

fn texture(ctx: &egui::Context, key: &'static str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(key);
    if let Some(tex) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return Some(tex);
    }
    let rgba = image::load_from_memory(bytes).ok()?.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw());
    let tex = ctx.load_texture(key, color, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    Some(tex)
}

fn wallpaper(ctx: &egui::Context, blurred: bool) -> Option<egui::TextureHandle> {
    if blurred {
        texture(ctx, "wall-blur", WALLPAPER_BLUR_JPG)
    } else {
        texture(ctx, "wall", WALLPAPER_JPG)
    }
}

fn cover_uv(tex_size: egui::Vec2, rect: egui::Rect, focus: (f32, f32)) -> egui::Rect {
    let scale = (rect.width() / tex_size.x).max(rect.height() / tex_size.y);
    let visible = egui::vec2(rect.width() / scale, rect.height() / scale);
    let free = tex_size - visible;
    let u0 = (free.x * focus.0) / tex_size.x;
    let v0 = (free.y * focus.1) / tex_size.y;
    egui::Rect::from_min_max(
        egui::pos2(u0, v0),
        egui::pos2(u0 + visible.x / tex_size.x, v0 + visible.y / tex_size.y),
    )
}

fn vertical_gradient(painter: &egui::Painter, rect: egui::Rect, stops: &[(f32, Color32)]) {
    let mut mesh = egui::Mesh::default();
    for (i, (t, color)) in stops.iter().enumerate() {
        let y = rect.min.y + rect.height() * t;
        let row = mesh.vertices.len() as u32;
        mesh.colored_vertex(egui::pos2(rect.min.x, y), *color);
        mesh.colored_vertex(egui::pos2(rect.max.x, y), *color);
        if i > 0 {
            mesh.add_triangle(row - 2, row - 1, row);
            mesh.add_triangle(row - 1, row, row + 1);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn horizontal_gradient(painter: &egui::Painter, rect: egui::Rect, stops: &[(f32, Color32)]) {
    let mut mesh = egui::Mesh::default();
    for (i, (t, color)) in stops.iter().enumerate() {
        let x = rect.min.x + rect.width() * t;
        let col = mesh.vertices.len() as u32;
        mesh.colored_vertex(egui::pos2(x, rect.min.y), *color);
        mesh.colored_vertex(egui::pos2(x, rect.max.y), *color);
        if i > 0 {
            mesh.add_triangle(col - 2, col - 1, col);
            mesh.add_triangle(col - 1, col, col + 1);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn scrim_color(alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(BG.r(), BG.g(), BG.b(), (alpha * 255.0).round() as u8)
}

fn panel_color(alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        PANEL_TINT.r(),
        PANEL_TINT.g(),
        PANEL_TINT.b(),
        (alpha * 255.0).round() as u8,
    )
}

fn paint_backdrop(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    scrim: Scrim,
    focus: (f32, f32),
) -> Option<egui::Rect> {
    let ctx = ui.ctx().clone();
    ui.painter().rect_filled(rect, CornerRadius::ZERO, BG);
    let uv = wallpaper(&ctx, false).map(|tex| {
        let uv = cover_uv(tex.size_vec2(), rect, focus);
        egui::Image::new(&tex).uv(uv).paint_at(ui, rect);
        uv
    });
    vertical_gradient(
        ui.painter(),
        rect,
        &[
            (0.0, scrim_color(scrim.top)),
            (1.0, scrim_color(scrim.bottom)),
        ],
    );
    uv
}

fn frost(
    ui: &mut egui::Ui,
    window: egui::Rect,
    panel: egui::Rect,
    uv: Option<egui::Rect>,
    scrim: Scrim,
) {
    if let (Some(uv), Some(tex)) = (uv, wallpaper(ui.ctx(), true)) {
        let fx = (panel.min.x - window.min.x) / window.width();
        let sub = egui::Rect::from_min_max(
            egui::pos2(uv.min.x + uv.width() * fx, uv.min.y),
            egui::pos2(uv.max.x, uv.max.y),
        );
        egui::Image::new(&tex).uv(sub).paint_at(ui, panel);
        vertical_gradient(
            ui.painter(),
            panel,
            &[
                (0.0, scrim_color(scrim.top)),
                (1.0, scrim_color(scrim.bottom)),
            ],
        );
    }
    horizontal_gradient(
        ui.painter(),
        panel,
        &[
            (0.0, panel_color(0.28)),
            (0.26, panel_color(0.82)),
            (1.0, panel_color(0.88)),
        ],
    );
    vertical_gradient(
        ui.painter(),
        egui::Rect::from_min_max(panel.min, egui::pos2(panel.min.x + 1.0, panel.max.y)),
        &[
            (0.0, Color32::TRANSPARENT),
            (0.16, tx(0.18)),
            (0.84, tx(0.18)),
            (1.0, Color32::TRANSPARENT),
        ],
    );
}

fn drag_strip(ui: &mut egui::Ui, ctx: &egui::Context, rect: egui::Rect, id: &'static str) {
    let response = ui.interact(rect, egui::Id::new(id), egui::Sense::DRAG);
    if response.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}

fn titlebar_button_rect(window: egui::Rect, slot: f32) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(window.max.x - 10.0 - 30.0 * (slot + 1.0), window.min.y + 9.0),
        egui::vec2(30.0, 24.0),
    )
}

fn minimize_button(ui: &mut egui::Ui, ctx: &egui::Context, window: egui::Rect) {
    let btn = titlebar_button_rect(window, 1.0);
    let response = ui.interact(btn, egui::Id::new("titlebar-minimize"), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Minimize"));
    let painter = ui.painter();
    let icon = if response.hovered() {
        painter.rect_filled(btn, CornerRadius::same(5), Color32::from_white_alpha(18));
        Color32::WHITE
    } else {
        tx(0.55)
    };
    let c = btn.center();
    painter.line_segment(
        [egui::pos2(c.x - 4.5, c.y), egui::pos2(c.x + 4.5, c.y)],
        egui::Stroke::new(1.4, icon),
    );
    focus_ring(painter, btn, 5, response.has_focus());
    if response.clicked() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    }
}

fn close_button(ui: &mut egui::Ui, ctx: &egui::Context, window: egui::Rect, enabled: bool) {
    let btn = titlebar_button_rect(window, 0.0);
    let response = ui.interact(
        btn,
        egui::Id::new("titlebar-close"),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, "Close"));
    let painter = ui.painter();
    let icon = if enabled && response.hovered() {
        painter.rect_filled(
            btn,
            CornerRadius::same(5),
            Color32::from_rgba_unmultiplied(0xe0, 0x65, 0x5f, 190),
        );
        Color32::WHITE
    } else if enabled {
        tx(0.55)
    } else {
        tx(0.22)
    };
    let c = btn.center();
    let r = 4.0;
    let stroke = egui::Stroke::new(1.4, icon);
    painter.line_segment(
        [egui::pos2(c.x - r, c.y - r), egui::pos2(c.x + r, c.y + r)],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(c.x - r, c.y + r), egui::pos2(c.x + r, c.y - r)],
        stroke,
    );
    focus_ring(painter, btn, 5, response.has_focus());
    if !enabled {
        response.on_hover_text("Setup is working. Please wait until it finishes.");
        return;
    }
    if response.clicked() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

const STEP_ROW: f32 = 17.0;
const STEP_LINK: f32 = 16.0;
const DOT: f32 = 7.0;

// ------------ Step Rail And Layouts ------------
// The left rail that shows Setup, Install and Ready, and the two page layouts built around it.
fn connector_alphas(step: Step, index: usize) -> (f32, f32) {
    match (step, index) {
        (Step::Setup, 0) => (0.30, 0.10),
        (Step::Setup, _) => (0.14, 0.06),
        (Step::Install, 0) => (0.14, 0.30),
        (Step::Install, _) => (0.14, 0.06),
        (Step::Ready, 0) => (0.14, 0.20),
        (Step::Ready, _) => (0.20, 0.30),
    }
}

fn step_row(ui: &egui::Ui, x: f32, y: f32, label: &str, color: Color32, filled: bool) {
    let painter = ui.painter();
    let center = egui::pos2(x + DOT / 2.0, y + STEP_ROW / 2.0);
    if filled {
        painter.circle_filled(center, DOT / 2.0, color);
    } else {
        painter.circle_stroke(center, DOT / 2.0 - 0.5, egui::Stroke::new(1.0, tx(0.30)));
    }
    let galley = ui.fonts(|f| {
        f.layout_no_wrap(
            label.to_owned(),
            egui::FontId::new(text::MD, egui::FontFamily::Proportional),
            color,
        )
    });
    painter.galley(
        egui::pos2(x + DOT + 11.0, y + (STEP_ROW - galley.size().y) / 2.0),
        galley,
        color,
    );
}

fn paint_rail(ui: &mut egui::Ui, rail_rect: egui::Rect, rail: &Rail) {
    let x = rail_rect.min.x + RAIL_PAD_X;
    let mut y = rail_rect.min.y + RAIL_PAD_TOP;

    if let Some(label) = rail.error {
        step_row(ui, x, y, label, DANGER, true);
    } else if let Some(active) = rail.step {
        for (i, (step, label)) in [
            (Step::Setup, "Setup"),
            (Step::Install, "Install"),
            (Step::Ready, "Ready"),
        ]
        .into_iter()
        .enumerate()
        {
            let on = step == active;
            step_row(
                ui,
                x,
                y,
                label,
                if on { Color32::WHITE } else { tx(0.40) },
                on,
            );
            y += STEP_ROW;
            if i < 2 {
                let (a, b) = connector_alphas(active, i);
                vertical_gradient(
                    ui.painter(),
                    egui::Rect::from_min_size(egui::pos2(x + 3.0, y), egui::vec2(1.0, STEP_LINK)),
                    &[(0.0, tx(a)), (1.0, tx(b))],
                );
                y += STEP_LINK;
            }
        }
    }

    const LOGO: f32 = 34.0;
    const NAME_H: f32 = 21.0;
    const IDENT_H: f32 = 15.0;
    let block_h = LOGO + 11.0 + NAME_H + IDENT_H;
    let top = rail_rect.max.y - RAIL_PAD_BOTTOM - block_h;

    if let Some(logo) = rail.logo {
        egui::Image::new(logo)
            .corner_radius(CornerRadius::same(9))
            .tint(Color32::WHITE.gamma_multiply(rail.logo_alpha))
            .paint_at(
                ui,
                egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(LOGO, LOGO)),
            );
    }

    let name = ui.fonts(|f| {
        f.layout_no_wrap(
            crate::consts::PRODUCT_NAME.to_owned(),
            egui::FontId::new(15.5, egui::FontFamily::Name(SEMIBOLD.into())),
            Color32::WHITE,
        )
    });
    ui.painter()
        .galley(egui::pos2(x, top + LOGO + 11.0), name, Color32::WHITE);

    let ident_color = tx(0.56);
    let ident = ui.fonts(|f| {
        f.layout_no_wrap(
            rail.identity.clone(),
            egui::FontId::new(text::SM, egui::FontFamily::Proportional),
            ident_color,
        )
    });
    ui.painter().galley(
        egui::pos2(x, top + LOGO + 11.0 + NAME_H),
        ident,
        ident_color,
    );
}

pub fn split(
    ctx: &egui::Context,
    surface: Surface,
    scrim: Scrim,
    closable: Close,
    rail: Rail,
    body: impl FnOnce(&mut egui::Ui),
) {
    let focus = match surface {
        Surface::Install => (0.32, 0.5),
        Surface::Uninstall => (0.38, 0.5),
    };
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            let uv = paint_backdrop(ui, rect, scrim, focus);

            let panel =
                egui::Rect::from_min_max(egui::pos2(rect.min.x + RAIL_W, rect.min.y), rect.max);
            frost(ui, rect, panel, uv, scrim);

            let rail_rect =
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + RAIL_W, rect.max.y));
            drag_strip(ui, ctx, rail_rect, "rail-drag");
            drag_strip(
                ui,
                ctx,
                egui::Rect::from_min_size(panel.min, egui::vec2(panel.width(), 42.0)),
                "titlebar-drag",
            );
            minimize_button(ui, ctx, rect);
            close_button(ui, ctx, rect, closable == Close::Enabled);

            paint_rail(ui, rail_rect, &rail);

            let inner = egui::Rect::from_min_max(
                panel.min + egui::vec2(PANEL_PAD_X, PANEL_PAD_TOP),
                panel.max - egui::vec2(PANEL_PAD_X, PANEL_PAD_BOTTOM),
            );
            let mut body_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(inner)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            body_ui.spacing_mut().item_spacing.y = 0.0;
            body(&mut body_ui);
        });
}

pub fn compact(
    ctx: &egui::Context,
    surface: Surface,
    closable: Close,
    body: impl FnOnce(&mut egui::Ui),
) {
    let focus = match surface {
        Surface::Install => (0.20, 0.40),
        Surface::Uninstall => (0.30, 0.45),
    };
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            let ctx_ref = ui.ctx().clone();
            ui.painter().rect_filled(rect, CornerRadius::ZERO, BG);
            if let Some(tex) = wallpaper(&ctx_ref, true) {
                let uv = cover_uv(tex.size_vec2(), rect, focus);
                egui::Image::new(&tex).uv(uv).paint_at(ui, rect);
            }
            horizontal_gradient(
                ui.painter(),
                rect,
                &[
                    (0.0, scrim_color(SCRIM_SPLASH.top)),
                    (1.0, scrim_color(SCRIM_SPLASH.bottom)),
                ],
            );

            drag_strip(
                ui,
                ctx,
                egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 42.0)),
                "titlebar-drag",
            );
            minimize_button(ui, ctx, rect);
            close_button(ui, ctx, rect, closable == Close::Enabled);

            let inner = egui::Rect::from_min_max(
                rect.min + egui::vec2(SPLASH_PAD_X, SPLASH_PAD_TOP),
                rect.max - egui::vec2(SPLASH_PAD_X, SPLASH_PAD_BOTTOM),
            );
            let mut body_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(inner)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            body_ui.spacing_mut().item_spacing.y = 0.0;
            body(&mut body_ui);
        });
}

// ------------ Shared Widgets ------------
// Text, cards, checkboxes, the folder field, the buttons and the progress bar.
pub fn footer(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let rect = ui.max_rect();
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::bottom_up(egui::Align::Min)),
    );
    child.spacing_mut().item_spacing.y = 0.0;
    add(&mut child);
}

pub fn action_row(ui: &mut egui::Ui, right_aligned: bool, add: impl FnOnce(&mut egui::Ui)) {
    let layout = if right_aligned {
        egui::Layout::right_to_left(egui::Align::Center)
    } else {
        egui::Layout::left_to_right(egui::Align::Center)
    };
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 38.0), layout, add);
}

fn paragraph_label(
    ui: &mut egui::Ui,
    body: &str,
    size: f32,
    color: Color32,
    max_width: f32,
    selectable: bool,
) {
    let width = max_width.min(ui.available_width());
    ui.allocate_ui(egui::vec2(width, 0.0), |ui| {
        ui.add(
            egui::Label::new(
                RichText::new(body)
                    .size(size)
                    .color(color)
                    .line_height(Some(size * 1.55)),
            )
            .wrap()
            .selectable(selectable),
        );
    });
}

pub fn paragraph(ui: &mut egui::Ui, body: &str, size: f32, color: Color32, max_width: f32) {
    paragraph_label(ui, body, size, color, max_width, false);
}

pub fn selectable_paragraph(
    ui: &mut egui::Ui,
    body: &str,
    size: f32,
    color: Color32,
    max_width: f32,
) {
    paragraph_label(ui, body, size, color, max_width, true);
}

pub fn meta_label(ui: &mut egui::Ui, label: &str) {
    ui.label(
        RichText::new(label.to_uppercase())
            .size(text::MICRO)
            .color(tx(0.55)),
    );
}

pub fn hairline(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    horizontal_gradient(
        ui.painter(),
        rect,
        &[
            (0.0, Color32::TRANSPARENT),
            (0.12, tx(0.14)),
            (0.88, tx(0.14)),
            (1.0, Color32::TRANSPARENT),
        ],
    );
}

pub fn bullet_row(ui: &mut egui::Ui, color: Color32, body: &str) {
    let width = ui.available_width();
    ui.horizontal_top(|row| {
        let line = text::SM * 1.5;
        let (dot, _) = row.allocate_exact_size(egui::vec2(5.0, line), egui::Sense::hover());
        row.painter().circle_filled(
            egui::pos2(dot.min.x + 2.5, dot.min.y + line / 2.0),
            2.5,
            color,
        );
        row.add_space(9.0);
        row.allocate_ui(egui::vec2(width - 14.0, 0.0), |col| {
            col.add(
                egui::Label::new(
                    RichText::new(body)
                        .size(text::SM)
                        .color(color)
                        .line_height(Some(text::SM * 1.5)),
                )
                .wrap(),
            );
        });
    });
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Notice,
    Danger,
    Plain,
}

impl Tone {
    fn colors(self) -> (Color32, Color32) {
        match self {
            Tone::Notice => (NOTICE.gamma_multiply(0.26), NOTICE.gamma_multiply(0.06)),
            Tone::Danger => (DANGER.gamma_multiply(0.24), DANGER.gamma_multiply(0.06)),
            Tone::Plain => (tx(0.12), tx(0.04)),
        }
    }
}

pub fn card(ui: &mut egui::Ui, tone: Tone, pad: i8, body: impl FnOnce(&mut egui::Ui)) {
    let (stroke, fill) = tone.colors();
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(pad))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
}

pub fn detail_box(ui: &mut egui::Ui, tone: Tone, label: &str, body: &str) {
    card(ui, tone, 12, |card| {
        meta_label(card, label);
        card.add_space(5.0);
        card.add(
            egui::Label::new(
                RichText::new(body)
                    .size(text::SM)
                    .color(tx(0.82))
                    .line_height(Some(text::SM * 1.55)),
            )
            .wrap()
            .selectable(true),
        );
    });
}

pub fn text_field(ui: &mut egui::Ui, value: &mut String, size: egui::Vec2) -> egui::Response {
    const RADIUS: u8 = 8;
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let id = ui.make_persistent_id("peebify-text-field");
    let focused = ui.memory(|m| m.has_focus(id));
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(RADIUS), tx(0.05));
    painter.rect_stroke(
        rect,
        CornerRadius::same(RADIUS),
        egui::Stroke::new(1.0, if focused { tx(0.55) } else { tx(0.16) }),
        egui::StrokeKind::Inside,
    );

    let inner = rect.shrink2(egui::vec2(11.0, 1.0));
    let mut field_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    field_ui.add_sized(
        inner.size(),
        egui::TextEdit::singleline(value)
            .id(id)
            .frame(false)
            .text_color(tx(0.85))
            .font(egui::FontId::new(text::MD, egui::FontFamily::Proportional))
            .vertical_align(egui::Align::Center)
            .margin(egui::Margin::ZERO),
    )
}

pub fn checkbox(ui: &mut egui::Ui, checked: &mut bool, label: &str) -> egui::Response {
    const BOX: f32 = 17.0;
    const GAP: f32 = 11.0;
    const RADIUS: u8 = 4;
    let font = egui::FontId::new(text::BASE, egui::FontFamily::Proportional);
    let color = tx(0.90);
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_owned(), font, color));
    let size = egui::vec2(BOX + GAP + galley.size().x, BOX.max(galley.size().y));
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, *checked, label)
    });
    let hover = ui
        .ctx()
        .animate_bool_with_time(response.id, response.hovered(), 0.12);
    let draw = ui
        .ctx()
        .animate_bool_with_time(response.id.with("check"), *checked, 0.12);

    let box_rect = egui::Rect::from_center_size(
        egui::pos2(rect.min.x + BOX / 2.0, rect.center().y),
        egui::vec2(BOX, BOX),
    );
    let painter = ui.painter();
    if *checked {
        painter.rect_filled(box_rect, CornerRadius::same(RADIUS), Color32::WHITE);
        let c = box_rect.center();
        let p1 = egui::pos2(c.x - 4.0, c.y + 0.3);
        let p2 = egui::pos2(c.x - 1.4, c.y + 2.9);
        let p3 = egui::pos2(c.x + 4.1, c.y - 3.1);
        let mut points = vec![p1];
        if draw < 0.5 {
            points.push(p1 + (p2 - p1) * (draw * 2.0));
        } else {
            points.push(p2);
            points.push(p2 + (p3 - p2) * ((draw - 0.5) * 2.0));
        }
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(2.0, Color32::from_rgb(0x12, 0x13, 0x1e)),
        ));
    } else {
        painter.rect_filled(
            box_rect,
            CornerRadius::same(RADIUS),
            Color32::from_white_alpha((18.0 * hover) as u8),
        );
        painter.rect_stroke(
            box_rect,
            CornerRadius::same(RADIUS),
            egui::Stroke::new(1.0, tx(0.32 + 0.28 * hover)),
            egui::StrokeKind::Inside,
        );
    }
    painter.galley(
        egui::pos2(
            box_rect.max.x + GAP,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        color,
    );
    focus_ring(painter, box_rect, RADIUS, response.has_focus());
    response
        .clone()
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response
}

pub fn sub_note(ui: &mut egui::Ui, body: &str) {
    ui.horizontal_top(|row| {
        row.add_space(28.0);
        let width = row.available_width();
        row.allocate_ui(egui::vec2(width, 0.0), |col| {
            col.add(
                egui::Label::new(
                    RichText::new(body)
                        .size(text::XS)
                        .color(tx(0.56))
                        .line_height(Some(text::XS * 1.5)),
                )
                .wrap(),
            );
        });
    });
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

#[allow(clippy::too_many_arguments)]
fn pill_button(
    ui: &mut egui::Ui,
    label: &str,
    text_color: Color32,
    fill: Color32,
    fill_hover: Color32,
    stroke_color: Color32,
    stroke_hover: Color32,
    h_pad: f32,
    height: f32,
) -> egui::Response {
    let font = egui::FontId::new(text::BODY, egui::FontFamily::Name(SEMIBOLD.into()));
    let enabled = ui.is_enabled();
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_owned(), font, text_color));
    let size = egui::vec2(galley.size().x + h_pad * 2.0, height);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));

    let t = ui
        .ctx()
        .animate_bool_with_time(response.id, enabled && response.hovered(), 0.12);
    let pressed = response.is_pointer_button_down_on();
    let rect = rect.translate(egui::vec2(0.0, if pressed { 1.0 } else { 0.0 }));

    let fill = lerp_color(fill, fill_hover, t);
    let stroke = lerp_color(stroke_color, stroke_hover, t);
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), fill);
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    let text_pos = rect.center() - galley.size() / 2.0;
    painter.galley(text_pos, galley, text_color);
    focus_ring(painter, rect, 8, response.has_focus());

    if enabled {
        response
            .clone()
            .on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    response
}

fn focus_ring(painter: &egui::Painter, rect: egui::Rect, radius: u8, focused: bool) {
    if !focused {
        return;
    }
    painter.rect_stroke(
        rect.expand(2.0),
        CornerRadius::same(radius + 2),
        egui::Stroke::new(1.5, tx(0.75)),
        egui::StrokeKind::Outside,
    );
}

pub fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    if !ui.is_enabled() {
        return secondary_button(ui, label);
    }
    pill_button(
        ui,
        label,
        Color32::from_rgb(0x12, 0x13, 0x1e),
        Color32::WHITE,
        tx(0.88),
        Color32::WHITE,
        Color32::WHITE,
        20.0,
        38.0,
    )
}

pub fn secondary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    pill_button(
        ui,
        label,
        tx(0.78),
        Color32::TRANSPARENT,
        Color32::from_white_alpha(14),
        tx(0.20),
        tx(0.42),
        20.0,
        38.0,
    )
}

pub fn small_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    pill_button(
        ui,
        label,
        tx(0.78),
        Color32::TRANSPARENT,
        Color32::from_white_alpha(14),
        tx(0.20),
        tx(0.42),
        15.0,
        36.0,
    )
}

pub fn danger_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    pill_button(
        ui,
        label,
        DANGER_TEXT,
        DANGER.gamma_multiply(0.09),
        DANGER.gamma_multiply(0.22),
        DANGER.gamma_multiply(0.70),
        DANGER,
        24.0,
        38.0,
    )
}

pub fn link(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let font = egui::FontId::new(12.0, egui::FontFamily::Proportional);
    let color = tx(0.60);
    let galley = ui.fonts(|f| f.layout_no_wrap(label.to_owned(), font, color));
    let (rect, response) =
        ui.allocate_exact_size(galley.size() + egui::vec2(0.0, 4.0), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, true, label));
    let hovered = response.hovered();
    let color = if hovered { tx(0.85) } else { color };
    let painter = ui.painter();
    painter.galley(rect.min, galley, color);
    painter.line_segment(
        [
            egui::pos2(rect.min.x, rect.max.y - 1.0),
            egui::pos2(rect.max.x, rect.max.y - 1.0),
        ],
        egui::Stroke::new(1.0, if hovered { tx(0.55) } else { tx(0.28) }),
    );
    focus_ring(painter, rect, 2, response.has_focus());
    response
        .clone()
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response
}

pub fn progress_bar(ui: &mut egui::Ui, fraction: f32) {
    const HEIGHT: f32 = 6.0;
    const RADIUS: u8 = 3;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), HEIGHT),
        egui::Sense::hover(),
    );
    ui.painter()
        .rect_filled(rect, CornerRadius::same(RADIUS), tx(0.10));

    if fraction < 0.0 {
        let width = rect.width() * 0.3;
        let travel = rect.width() + width;
        let t = (ui.input(|i| i.time) % 1.4) as f32 / 1.4;
        let x = rect.min.x - width + travel * t;
        let band = egui::Rect::from_min_max(
            egui::pos2(x.max(rect.min.x), rect.min.y),
            egui::pos2((x + width).min(rect.max.x), rect.max.y),
        );
        if band.width() > 0.5 {
            ui.painter()
                .rect_filled(band, CornerRadius::same(RADIUS), Color32::WHITE);
        }
        ui.ctx().request_repaint();
        return;
    }

    let eased =
        ui.ctx()
            .animate_value_with_time(ui.id().with("progress"), fraction.clamp(0.0, 1.0), 0.35);
    if eased > 0.0 {
        let mut fill = rect;
        fill.set_width((rect.width() * eased).max(HEIGHT));
        ui.painter()
            .rect_filled(fill, CornerRadius::same(RADIUS), Color32::WHITE);
    }
}

// ------------ Progress Pacing And Engine Thread ------------
// Pacer holds each progress phase on screen for a moment so fast ones do not flicker past. spawn_engine runs the
// install or uninstall work on a thread, and friendly_error turns raw OS errors into plain advice.
pub struct Pacer {
    phase: String,
    percent: f32,
    queue: std::collections::VecDeque<(String, f32)>,
    shown_at: std::time::Instant,
    finished: bool,
}

const PHASE_DWELL: std::time::Duration = std::time::Duration::from_millis(420);

impl Pacer {
    pub fn new(initial: impl Into<String>) -> Self {
        Self {
            phase: initial.into(),
            percent: 0.0,
            queue: std::collections::VecDeque::new(),
            shown_at: std::time::Instant::now(),
            finished: false,
        }
    }

    pub fn push(&mut self, phase: String, percent: f32) {
        let current = self.queue.back().map(|(p, _)| p).unwrap_or(&self.phase);
        if current == &phase {
            match self.queue.back_mut() {
                Some(last) => last.1 = percent,
                None => self.percent = percent,
            }
            return;
        }
        self.queue.push_back((phase, percent));
    }

    pub fn finish(&mut self) {
        self.finished = true;
        let last = self.queue.back().map(|(_, p)| *p).unwrap_or(self.percent);
        if last < 100.0 {
            self.push("Finishing up…".to_string(), 100.0);
        }
        match self.queue.back_mut() {
            Some(last) => last.1 = 100.0,
            None => self.percent = 100.0,
        }
    }

    pub fn tick(&mut self) {
        while self.shown_at.elapsed() >= PHASE_DWELL {
            let Some((phase, percent)) = self.queue.pop_front() else {
                break;
            };
            self.phase = phase;
            self.percent = percent;
            self.shown_at = std::time::Instant::now();
        }
    }

    pub fn phase(&self) -> &str {
        &self.phase
    }

    pub fn percent(&self) -> f32 {
        self.percent
    }

    pub fn settled(&self) -> bool {
        self.finished && self.queue.is_empty() && self.shown_at.elapsed() >= PHASE_DWELL
    }
}

pub fn spawn_engine(
    work: impl FnOnce(&crate::msg::EventSink) -> Result<(), crate::msg::EngineError> + Send + 'static,
) -> std::sync::mpsc::Receiver<crate::msg::EngineEvent> {
    use crate::msg::{EngineError, EngineEvent};
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = work(&tx);
        let _ = match result {
            Ok(()) => tx.send(EngineEvent::Finished),
            Err(EngineError::Cancelled) => tx.send(EngineEvent::Cancelled),
            Err(EngineError::Failed(e)) => tx.send(EngineEvent::Failed(e)),
        };
    });
    rx
}

fn has_os_error(lower: &str, code: u32) -> bool {
    lower.match_indices("os error ").any(|(at, needle)| {
        let digits: String = lower[at + needle.len()..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse::<u32>() == Ok(code)
    })
}

pub fn friendly_error(raw: &str) -> &'static str {
    let lower = raw.to_lowercase();
    if has_os_error(&lower, 5) || lower.contains("access is denied") {
        "Windows or security software blocked a file setup needed. Close Peebify \
         Launcher and any running game, allow setup in your antivirus, or choose a \
         different folder, then try again."
    } else if has_os_error(&lower, 112) || lower.contains("not enough space") {
        "The drive ran out of space partway through. \
         Free some room, or install to another drive, and try again."
    } else if has_os_error(&lower, 32) || lower.contains("being used by another process") {
        "A file setup needed was in use. Close Peebify Launcher and any running \
         game, then try again."
    } else if lower.contains("checksum") || lower.contains("corrupt") {
        "The download is damaged. Download setup again from peebify.net. \
         A partial download is the usual cause."
    } else {
        ""
    }
}

pub fn bar_fraction(percent: f32) -> f32 {
    if percent < 0.0 {
        percent
    } else {
        percent / 100.0
    }
}

pub fn log_dir() -> Option<std::path::PathBuf> {
    crate::consts::user_data_dir().map(|d| d.join("logs"))
}

#[cfg(test)]
mod tests {
    use super::{friendly_error, Pacer};

    #[test]
    fn access_denied_never_suggests_administrator() {
        for raw in ["Access is denied. (os error 5)", "rename failed: ACCESS IS DENIED"] {
            let msg = friendly_error(raw);
            assert!(msg.contains("security software"));
            assert!(!msg.to_lowercase().contains("administrator"));
        }
    }

    #[test]
    fn os_error_codes_match_exactly() {
        for raw in [
            "copy failed: The network path was not found. (os error 53)",
            "read failed: An unexpected network error occurred. (os error 59)",
            "write failed (os error 320)",
            "write failed (os error 1120)",
        ] {
            assert_eq!(friendly_error(raw), "", "{raw}");
        }
        assert!(friendly_error("write failed (os error 112)").contains("ran out of space"));
        assert!(friendly_error("rename failed (os error 32)").contains("in use"));
        assert!(friendly_error("os error 5").contains("security software"));
    }

    #[test]
    fn finish_does_not_follow_done_with_finishing_up() {
        let mut pacer = Pacer::new("Preparing…");
        pacer.push("Done".to_string(), 100.0);
        pacer.finish();
        assert_eq!(pacer.queue.back().map(|(p, _)| p.as_str()), Some("Done"));

        let mut pacer = Pacer::new("Preparing…");
        pacer.push("Copying…".to_string(), 60.0);
        pacer.finish();
        assert_eq!(
            pacer.queue.back().map(|(p, pct)| (p.as_str(), *pct)),
            Some(("Finishing up…", 100.0))
        );
    }
}
