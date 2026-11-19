#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(not(windows))]
fn main() {
    eprintln!("The desktop app is currently available on Windows.");
}
#[cfg(windows)]
mod app {
    use eframe::egui::{self, Color32, RichText};
    use g502_rs::device::{battery, dpi, polling};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use serde::{Deserialize, Serialize};
    use std::{
        fs,
        path::PathBuf,
        sync::mpsc::{self, Receiver, Sender},
        time::{Duration, Instant},
    };
    use tray_icon::{
        MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem},
    };

    #[derive(Serialize, Deserialize)]
    #[serde(default)]
    struct Settings {
        dark: bool,
        startup_initialized: bool,
    }
    impl Default for Settings {
        fn default() -> Self {
            Self {
                dark: true,
                startup_initialized: false,
            }
        }
    }
    fn settings_path() -> PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("g502-rs")
            .join("settings.json")
    }
    fn save_settings(s: &Settings) -> Result<(), String> {
        let p = settings_path();
        fs::create_dir_all(p.parent().ok_or("Invalid settings path")?)
            .map_err(|e| e.to_string())?;
        fs::write(p, serde_json::to_vec(s).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
    }
    fn startup_enabled() -> bool {
        use winreg::{RegKey, enums::*};
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
            .ok()
            .and_then(|k| k.get_value::<String, _>("G502 Mouse Manager").ok())
            .is_some()
    }
    fn set_startup(enabled: bool) -> Result<(), String> {
        use winreg::{RegKey, enums::*};
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
            .map_err(|e| e.to_string())?;
        if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            key.set_value(
                "G502 Mouse Manager",
                &format!("\"{}\" --background", exe.display()),
            )
            .map_err(|e| e.to_string())
        } else {
            match key.delete_value("G502 Mouse Manager") {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        }
    }
    enum Command {
        Refresh,
        SetDpi(u16, u8),
        SetPolling(u8),
    }
    enum Message {
        Battery(Result<battery::BatteryStatus, String>),
        Dpi(Result<u16, String>),
        Polling(Result<polling::Polling, String>),
        Finished(Option<String>),
    }
    fn worker(rx: Receiver<Command>, tx: Sender<Message>) {
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Command::Refresh => {
                    match g502_rs::open_mouse() {
                        Ok(mut t) => {
                            let _ = tx.send(Message::Battery(
                                battery::read(&mut t).map_err(|e| e.to_string()),
                            ));
                            let _ =
                                tx.send(Message::Dpi(dpi::get(&mut t).map_err(|e| e.to_string())));
                            let _ = tx.send(Message::Polling(
                                polling::get(&mut t).map_err(|e| e.to_string()),
                            ));
                        }
                        Err(e) => {
                            let s = e.to_string();
                            let _ = tx.send(Message::Battery(Err(s.clone())));
                            let _ = tx.send(Message::Dpi(Err(s.clone())));
                            let _ = tx.send(Message::Polling(Err(s)));
                        }
                    }
                    let _ = tx.send(Message::Finished(None));
                }
                Command::SetDpi(value, level) => {
                    let result = g502_rs::open_mouse()
                        .and_then(|mut t| dpi::set(&mut t, value, level))
                        .map_err(|e| e.to_string());
                    let error = result.as_ref().err().cloned();
                    let _ = tx.send(Message::Dpi(result));
                    let _ = tx.send(Message::Finished(error));
                }
                Command::SetPolling(ms) => {
                    let result = g502_rs::open_mouse()
                        .and_then(|mut t| polling::set(&mut t, ms))
                        .map_err(|e| e.to_string());
                    let error = result.as_ref().err().cloned();
                    let _ = tx.send(Message::Polling(result));
                    let _ = tx.send(Message::Finished(error));
                }
            }
        }
    }
    fn icon(value: Option<u8>) -> tray_icon::Icon {
        use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
        use std::sync::OnceLock;
        static FONT: OnceLock<Option<FontArc>> = OnceLock::new();
        let font = FONT.get_or_init(|| {
            let root = std::env::var_os("WINDIR").map(PathBuf::from)?;
            let data = fs::read(root.join("Fonts").join("seguisb.ttf")).ok()?;
            FontArc::try_from_vec(data).ok()
        });
        let text = value
            .filter(|v| *v <= 100)
            .map(|v| v.to_string())
            .unwrap_or("?".into());
        let mut alpha = vec![0u8; 64 * 64];
        if let Some(font) = font {
            // Fit the ink bounds rather than the advance width so every value,
            // including 100, remains centered and uses the available space.
            let layout = |size: f32| {
                let scaled = font.as_scaled(PxScale::from(size));
                let mut x = 0.0;
                let mut glyphs = Vec::new();
                for c in text.chars() {
                    let id = font.glyph_id(c);
                    let glyph =
                        id.with_scale_and_position(PxScale::from(size), point(x, scaled.ascent()));
                    x += scaled.h_advance(id);
                    if let Some(outline) = font.outline_glyph(glyph) {
                        glyphs.push(outline);
                    }
                }
                glyphs
            };
            let first = layout(54.0);
            let bounds = |glyphs: &[ab_glyph::OutlinedGlyph]| {
                glyphs.iter().map(|g| g.px_bounds()).fold(
                    (
                        f32::INFINITY,
                        f32::INFINITY,
                        f32::NEG_INFINITY,
                        f32::NEG_INFINITY,
                    ),
                    |(x0, y0, x1, y1), b| {
                        (
                            x0.min(b.min.x),
                            y0.min(b.min.y),
                            x1.max(b.max.x),
                            y1.max(b.max.y),
                        )
                    },
                )
            };
            let (x0, y0, x1, y1) = bounds(&first);
            if x0.is_finite() {
                let factor = (58.0 / (x1 - x0)).min(52.0 / (y1 - y0)).min(1.0);
                let glyphs = layout(54.0 * factor);
                let (x0, y0, x1, y1) = bounds(&glyphs);
                let left = ((64.0 - (x1 - x0)) / 2.0 - x0).round() as i32;
                let top = ((64.0 - (y1 - y0)) / 2.0 - y0).round() as i32;
                for glyph in glyphs {
                    let b = glyph.px_bounds();
                    glyph.draw(|x, y, coverage| {
                        let x = left + b.min.x as i32 + x as i32;
                        let y = top + b.min.y as i32 + y as i32;
                        if (0..64).contains(&x) && (0..64).contains(&y) {
                            let i = y as usize * 64 + x as usize;
                            alpha[i] = alpha[i].max((coverage * 255.0) as u8);
                        }
                    });
                }
            }
        } else {
            // Built-in fallback if the Windows font cannot be read.
            const GLYPHS: [[u8; 5]; 11] = [
                [7, 5, 5, 5, 7],
                [2, 6, 2, 2, 7],
                [7, 1, 7, 4, 7],
                [7, 1, 7, 1, 7],
                [5, 5, 7, 1, 1],
                [7, 4, 7, 1, 7],
                [7, 4, 7, 5, 7],
                [7, 1, 1, 1, 1],
                [7, 5, 7, 5, 7],
                [7, 5, 7, 1, 7],
                [7, 1, 2, 0, 2],
            ];
            let scale = if text.len() == 3 { 5 } else { 7 };
            let start = (64 - (text.len() * 4 - 1) * scale) / 2;
            let top = (64 - 5 * scale) / 2;
            for (n, ch) in text.chars().enumerate() {
                for (y, row) in GLYPHS[ch.to_digit(10).map_or(10, |d| d as usize)]
                    .iter()
                    .enumerate()
                {
                    for x in 0..3 {
                        if row & (1 << (2 - x)) != 0 {
                            for dy in 0..scale {
                                for dx in 0..scale {
                                    alpha[(top + y * scale + dy) * 64
                                        + start
                                        + n * 4 * scale
                                        + x * scale
                                        + dx] = 255;
                                }
                            }
                        }
                    }
                }
            }
        }
        // A subtle dark edge keeps the white number legible on light taskbars.
        let mut pixels = vec![0u8; 64 * 64 * 4];
        for y in 0..64usize {
            for x in 0..64usize {
                let a = alpha[y * 64 + x];
                let mut edge = 0u8;
                for yy in y.saturating_sub(2)..=(y + 2).min(63) {
                    for xx in x.saturating_sub(2)..=(x + 2).min(63) {
                        edge = edge.max(alpha[yy * 64 + xx]);
                    }
                }
                let i = (y * 64 + x) * 4;
                if a > 0 {
                    pixels[i..i + 4].copy_from_slice(&[255, 255, 255, a]);
                } else if edge > 0 {
                    pixels[i..i + 4].copy_from_slice(&[0, 0, 0, edge]);
                }
            }
        }
        tray_icon::Icon::from_rgba(pixels, 64, 64).expect("valid generated icon")
    }
    fn wake_window(hwnd: Option<isize>) {
        if let Some(hwnd) = hwnd {
            // The handle belongs to this process and remains valid until eframe exits.
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
                    hwnd as windows_sys::Win32::Foundation::HWND,
                    windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE,
                );
            }
        }
    }
    enum TrayAction {
        Show,
        Quit,
    }
    struct App {
        settings: Settings,
        startup: bool,
        tray: Option<TrayIcon>,
        tray_events: Receiver<TrayAction>,
        tx: Sender<Command>,
        rx: Receiver<Message>,
        busy: bool,
        last_refresh: Instant,
        battery: Option<battery::BatteryStatus>,
        dpi: Option<u16>,
        dpi_input: u16,
        level: u8,
        polling: Option<polling::Polling>,
        selected_ms: u8,
        error: Option<String>,
        quitting: bool,
    }
    impl App {
        fn new(ctx: &eframe::CreationContext<'_>) -> Self {
            let mut settings: Settings = fs::read(settings_path())
                .ok()
                .and_then(|s| serde_json::from_slice(&s).ok())
                .unwrap_or_default();
            let mut error = None;
            if !settings.startup_initialized {
                match set_startup(true) {
                    Ok(()) => {
                        settings.startup_initialized = true;
                        if let Err(e) = save_settings(&settings) {
                            error = Some(e);
                        }
                    }
                    Err(e) => error = Some(format!("Could not enable startup: {e}")),
                }
            }
            let startup = startup_enabled();
            let menu = Menu::new();
            let show = MenuItem::new("Open mouse settings", true, None);
            let quit = MenuItem::new("Quit", true, None);
            let _ = menu.append_items(&[&show, &quit]);
            // Windows does not always repaint a hidden eframe viewport. Show its
            // native window before asking egui to process a tray action.
            let hwnd = ctx
                .window_handle()
                .ok()
                .and_then(|handle| match handle.as_raw() {
                    RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                    _ => None,
                });
            let (tray_tx, tray_events) = mpsc::channel();
            let show_id = show.id().clone();
            let quit_id = quit.id().clone();
            let menu_tx = tray_tx.clone();
            let menu_ctx = ctx.egui_ctx.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let action = if event.id == show_id {
                    Some(TrayAction::Show)
                } else if event.id == quit_id {
                    Some(TrayAction::Quit)
                } else {
                    None
                };
                if let Some(action) = action {
                    let _ = menu_tx.send(action);
                    wake_window(hwnd);
                    menu_ctx.request_repaint();
                }
            }));
            let tray_ctx = ctx.egui_ctx.clone();
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                ) {
                    let _ = tray_tx.send(TrayAction::Show);
                    wake_window(hwnd);
                    tray_ctx.request_repaint();
                }
            }));
            let tray = match TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("Mouse battery: unknown")
                .with_icon(icon(None))
                .build()
            {
                Ok(t) => Some(t),
                Err(e) => {
                    error = Some(format!("Could not create tray icon: {e}"));
                    None
                }
            };
            if std::env::args().any(|a| a == "--background") && tray.is_some() {
                ctx.egui_ctx
                    .send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
            let (tx, command_rx) = mpsc::channel();
            let (result_tx, rx) = mpsc::channel();
            std::thread::spawn(move || worker(command_rx, result_tx));
            let _ = tx.send(Command::Refresh);
            Self {
                settings,
                startup,
                tray,
                tray_events,
                tx,
                rx,
                busy: true,
                last_refresh: Instant::now(),
                battery: None,
                dpi: None,
                dpi_input: 1600,
                level: 1,
                polling: None,
                selected_ms: 1,
                error,
                quitting: false,
            }
        }
        fn send(&mut self, cmd: Command) {
            if !self.busy {
                if self.tx.send(cmd).is_ok() {
                    self.busy = true;
                    self.error = None;
                } else {
                    self.error = Some("Device worker stopped".into());
                }
            }
        }
        fn show(&self, ctx: &egui::Context) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }
    impl eframe::App for App {
        fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
            ctx.request_repaint_after(Duration::from_millis(250));
            while let Ok(action) = self.tray_events.try_recv() {
                match action {
                    TrayAction::Show => self.show(ctx),
                    TrayAction::Quit => {
                        self.quitting = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            if ctx.input(|i| i.viewport().close_requested())
                && !self.quitting
                && self.tray.is_some()
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
            while let Ok(msg) = self.rx.try_recv() {
                match msg {
                    Message::Battery(Ok(b)) => {
                        if let Some(t) = &self.tray {
                            let _ = t.set_icon(Some(icon(b.percentage)));
                            let _ = t.set_tooltip(Some(
                                b.percentage
                                    .map(|v| format!("Mouse battery: {v}%"))
                                    .unwrap_or("Mouse battery: unavailable".into()),
                            ));
                        }
                        self.battery = Some(b);
                    }
                    Message::Battery(Err(e)) => {
                        self.battery = None;
                        if let Some(t) = &self.tray {
                            let _ = t.set_icon(Some(icon(None)));
                            let _ = t.set_tooltip(Some("Mouse battery: unavailable"));
                        }
                        self.error = Some(e);
                    }
                    Message::Dpi(Ok(v)) => {
                        self.dpi = Some(v);
                        self.dpi_input = v;
                    }
                    Message::Dpi(Err(e)) => {
                        self.dpi = None;
                        self.error = Some(e);
                    }
                    Message::Polling(Ok(p)) => {
                        self.selected_ms = p.current_ms;
                        self.polling = Some(p);
                    }
                    Message::Polling(Err(e)) => {
                        self.polling = None;
                        self.error = Some(e);
                    }
                    Message::Finished(e) => {
                        self.busy = false;
                        if e.is_some() {
                            self.error = e;
                        }
                    }
                }
            }
            if !self.busy && self.last_refresh.elapsed() >= Duration::from_secs(60) {
                self.last_refresh = Instant::now();
                self.send(Command::Refresh);
            }
            ctx.set_visuals(if self.settings.dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.heading("Mouse settings");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(if self.settings.dark { "Light mode" } else { "Dark mode" }).clicked() {
                            self.settings.dark = !self.settings.dark;
                            if let Err(e) = save_settings(&self.settings) { self.error = Some(e); }
                        }
                    });
                });
                ui.label(RichText::new("LOGITECH G502 X PLUS").small().color(Color32::from_rgb(104,190,153)));
                ui.add_space(16.0);
                ui.group(|ui| {
                    ui.set_min_width(ui.available_width() - 10.0);
                    ui.label("BATTERY");
                    let text = self.battery.as_ref().and_then(|b| b.percentage).map(|v| format!("{v}%")).unwrap_or("Unavailable".into());
                    ui.heading(text);
                    if let Some(b) = &self.battery {
                        ui.label(match b.state { 0 => "Discharging", 1 | 2 | 4 => "Charging", 3 => "Full", _ => "Battery status unavailable" });
                    }
                });
                ui.add_space(10.0);
                ui.group(|ui| {
                    ui.set_min_width(ui.available_width() - 10.0);
                    ui.label("SENSITIVITY");
                    ui.label(self.dpi.map(|v| format!("Current: {v} DPI")).unwrap_or("Current DPI unavailable".into()));
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.dpi_input).range(100..=25600).speed(50).suffix(" DPI"));
                        ui.label("Level");
                        ui.add(egui::DragValue::new(&mut self.level).range(1..=5));
                        if ui.add_enabled(!self.busy && self.dpi.is_some(), egui::Button::new("Apply")).clicked() {
                            self.send(Command::SetDpi(self.dpi_input, self.level));
                        }
                    });
                    ui.small("Level is required by the observed DPI command; its meaning is still being studied.");
                });
                ui.add_space(10.0);
                ui.group(|ui| {
                    ui.set_min_width(ui.available_width() - 10.0);
                    ui.label("POLLING RATE");
                    if let Some(p) = &self.polling {
                        let intervals = p.intervals_ms.clone();
                        egui::ComboBox::from_id_salt("polling").selected_text(format!("{} Hz ({} ms)", 1000 / self.selected_ms.max(1) as u32, self.selected_ms)).show_ui(ui, |ui| {
                            for ms in intervals { ui.selectable_value(&mut self.selected_ms, ms, format!("{} Hz ({ms} ms)", 1000 / ms as u32)); }
                        });
                        if ui.add_enabled(!self.busy && self.selected_ms != p.current_ms, egui::Button::new("Apply polling rate")).clicked() { self.send(Command::SetPolling(self.selected_ms)); }
                    } else { ui.label("Unavailable on this connection"); }
                });
                ui.add_space(10.0);
                let mut start = self.startup;
                if ui.checkbox(&mut start, "Start with Windows").changed() {
                    match set_startup(start) { Ok(()) => self.startup = start, Err(e) => self.error = Some(format!("Startup setting: {e}")) }
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.busy, egui::Button::new("Refresh")).clicked() {
                        self.last_refresh = Instant::now(); self.send(Command::Refresh);
                    }
                    if self.busy { ui.spinner(); ui.label("Connecting..."); }
                });
                if let Some(e) = &self.error { ui.colored_label(Color32::from_rgb(220,112,105), e); }
            });
        }
    }
    pub fn run() -> eframe::Result {
        eframe::run_native(
            "G502 Mouse Manager",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_inner_size([460.0, 610.0])
                    .with_min_inner_size([400.0, 570.0]),
                ..Default::default()
            },
            Box::new(|ctx| Ok(Box::new(App::new(ctx)))),
        )
    }
}
#[cfg(windows)]
fn main() -> eframe::Result {
    app::run()
}
