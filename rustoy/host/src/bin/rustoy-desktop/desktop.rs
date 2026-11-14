use eframe::egui::{self, Color32, RichText};
#[cfg(windows)]
use rustoy_host::usb::{self, Drive};
use std::sync::mpsc;

const BACKGROUND: Color32 = Color32::from_rgb(15, 21, 29);
const SURFACE: Color32 = Color32::from_rgb(24, 34, 46);
const BORDER: Color32 = Color32::from_rgb(49, 66, 83);
const ACCENT: Color32 = Color32::from_rgb(108, 229, 194);
const MUTED: Color32 = Color32::from_rgb(163, 181, 198);
#[cfg(windows)]
const WARNING: Color32 = Color32::from_rgb(255, 191, 124);
const ERROR: Color32 = Color32::from_rgb(255, 149, 151);

pub fn run() -> eframe::Result {
    eframe::run_native(
        "Rustoy",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([760.0, 600.0])
                .with_min_inner_size([460.0, 400.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            let mut visuals = egui::Visuals::dark();
            visuals.panel_fill = BACKGROUND;
            visuals.extreme_bg_color = BACKGROUND;
            visuals.selection.bg_fill = Color32::from_rgb(32, 93, 81);
            cc.egui_ctx.set_visuals(visuals);
            cc.egui_ctx.style_mut(|style| {
                style.spacing.item_spacing = egui::vec2(10.0, 12.0);
                style.spacing.button_padding = egui::vec2(14.0, 8.0);
            });
            Ok(Box::<Desktop>::default())
        }),
    )
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Page {
    #[cfg(windows)]
    #[default]
    Install,
    #[cfg_attr(not(windows), default)]
    Inspect,
}

enum Work {
    #[cfg(windows)]
    Drives(Result<Vec<Drive>, String>),
    #[cfg(windows)]
    Installed(Result<String, String>),
    Inspected(Result<String, String>),
}

#[derive(Default)]
struct Desktop {
    page: Page,
    #[cfg(windows)]
    started: bool,
    #[cfg(windows)]
    drives: Vec<Drive>,
    #[cfg(windows)]
    selected: Option<usize>,
    #[cfg(windows)]
    confirm: bool,
    #[cfg(windows)]
    install_result: Option<Result<String, String>>,
    image_path: String,
    image_result: Option<Result<String, String>>,
    pending: Option<(&'static str, mpsc::Receiver<Work>)>,
}

impl Desktop {
    fn run_work(
        &mut self,
        ctx: &egui::Context,
        status: &'static str,
        operation: impl FnOnce() -> Work + Send + 'static,
    ) {
        let (send, receive) = mpsc::channel();
        let ctx = ctx.clone();
        self.pending = Some((status, receive));
        std::thread::spawn(move || {
            let _ = send.send(operation());
            ctx.request_repaint();
        });
    }

    fn poll_work(&mut self) {
        let Some((_, receiver)) = &self.pending else {
            return;
        };
        let result = receiver.try_recv();
        if matches!(result, Err(mpsc::TryRecvError::Empty)) {
            return;
        }
        self.pending = None;
        match result {
            #[cfg(windows)]
            Ok(Work::Drives(Ok(drives))) => self.drives = drives,
            #[cfg(windows)]
            Ok(Work::Drives(Err(err))) => self.install_result = Some(Err(err)),
            #[cfg(windows)]
            Ok(Work::Installed(result)) => self.install_result = Some(result),
            Ok(Work::Inspected(result)) => self.image_result = Some(result),
            Err(_) => {
                let result = Err("The background operation ended unexpectedly.".into());
                match self.page {
                    #[cfg(windows)]
                    Page::Install => self.install_result = Some(result),
                    Page::Inspect => self.image_result = Some(result),
                }
            }
        }
    }

    fn inspect(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Inspect a disk image").size(23.0).strong());
            ui.label(
                RichText::new("Check the GPT layout, exFAT volume, and ISO files in a disk image.")
                    .color(MUTED),
            );

            ui.strong("Disk image path");
            if let Some(path) = ui.input(|input| {
                input
                    .raw
                    .dropped_files
                    .iter()
                    .find_map(|file| file.path.clone())
            }) {
                self.image_path = path.to_string_lossy().into_owned();
            }
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.image_path)
                    .hint_text("Enter a path or drop an image here")
                    .desired_width(f32::INFINITY),
            );
            let path = self.image_path.trim().trim_matches('"');
            let submit = ui
                .add_enabled(!path.is_empty(), primary_button("Inspect image"))
                .clicked()
                || (!path.is_empty()
                    && field.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if submit {
                let path = std::path::PathBuf::from(path);
                self.image_result = None;
                self.run_work(ui.ctx(), "Reading disk image…", move || {
                    Work::Inspected(rustoy_host::inspect_disk(path.as_os_str(), 512))
                });
            }
        });
    }

    #[cfg(windows)]
    fn refresh_drives(&mut self, ctx: &egui::Context) {
        self.drives.clear();
        self.selected = None;
        self.install_result = None;
        self.run_work(ctx, "Looking for USB drives…", || {
            Work::Drives(usb::list())
        });
    }

    #[cfg(windows)]
    fn install(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Install to USB").size(23.0).strong());
            ui.label(
                RichText::new("Prepare an x86-64 UEFI drive with an exFAT volume for your ISOs.")
                    .color(MUTED),
            );
            ui.strong("USB drive");
            ui.horizontal_wrapped(|ui| {
                let label = self
                    .selected
                    .and_then(|index| self.drives.get(index))
                    .map(drive_label)
                    .unwrap_or_else(|| "Select a USB drive".into());
                egui::ComboBox::from_id_salt("usb-drive")
                    .width((ui.available_width() - 100.0).clamp(150.0, 420.0))
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        for (index, drive) in self.drives.iter().enumerate() {
                            ui.selectable_value(
                                &mut self.selected,
                                Some(index),
                                drive_label(drive),
                            );
                        }
                    });
                if ui.button("Refresh").clicked() {
                    self.refresh_drives(ui.ctx());
                }
            });
            if self.drives.is_empty() && self.pending.is_none() && self.install_result.is_none() {
                ui.label(RichText::new("Connect a USB drive and select Refresh.").color(MUTED));
            }
            ui.colored_label(
                WARNING,
                "Installing erases all data on the selected USB drive.",
            );
            if ui
                .add_enabled(self.selected.is_some(), primary_button("Install Rustoy…"))
                .clicked()
            {
                self.confirm = true;
            }
        });
        ui.label(RichText::new("After installing, copy ISOs to the root of the RUSTOY volume. ISO boot is under development.").color(MUTED));
    }

    #[cfg(windows)]
    fn confirm_install(&mut self, ctx: &egui::Context) {
        if !self.confirm {
            return;
        }
        let Some(drive) = self
            .selected
            .and_then(|index| self.drives.get(index))
            .cloned()
        else {
            self.confirm = false;
            return;
        };
        let response = egui::Modal::new(egui::Id::new("confirm-install")).show(ctx, |ui| {
            ui.heading("Erase USB drive?");
            ui.label(drive_label(&drive));
            ui.colored_label(
                WARNING,
                "All partitions and files on this drive will be erased.",
            );
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    self.confirm = false;
                }
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Erase and install")
                                .strong()
                                .color(BACKGROUND),
                        )
                        .fill(WARNING),
                    )
                    .clicked()
                {
                    self.confirm = false;
                    self.selected = None;
                    self.install_result = None;
                    self.run_work(
                        ctx,
                        "Installing Rustoy… Keep the USB drive connected.",
                        move || Work::Installed(usb::install(drive)),
                    );
                }
            });
        });
        if response.should_close() {
            self.confirm = false;
        }
    }
}

impl eframe::App for Desktop {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        #[cfg(windows)]
        if !self.started {
            self.started = true;
            self.refresh_drives(ctx);
        }
        self.poll_work();
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Frame::NONE
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| {
                        ui.set_max_width(800.0);
                        ui.label(RichText::new("Rustoy").size(40.0).strong().color(ACCENT));
                        #[cfg(windows)]
                        ui.add_enabled_ui(self.pending.is_none() && !self.confirm, |ui| {
                            ui.horizontal(|ui| {
                                ui.selectable_value(
                                    &mut self.page,
                                    Page::Install,
                                    "Install to USB",
                                );
                                ui.selectable_value(&mut self.page, Page::Inspect, "Inspect image");
                            });
                        });
                        ui.add_enabled_ui(self.pending.is_none(), |ui| match self.page {
                            #[cfg(windows)]
                            Page::Install => self.install(ui),
                            Page::Inspect => self.inspect(ui),
                        });
                        if let Some((status, _)) = self.pending {
                            ctx.request_repaint_after(std::time::Duration::from_millis(250));
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(status);
                            });
                        }
                        let result = match self.page {
                            #[cfg(windows)]
                            Page::Install => &self.install_result,
                            Page::Inspect => &self.image_result,
                        };
                        if let Some(result) = result {
                            card().show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                match result {
                                    Ok(message) if self.page == Page::Inspect => {
                                        ui.strong("Image report");
                                        ui.monospace(message);
                                    }
                                    Ok(message) => {
                                        ui.colored_label(ACCENT, message);
                                    }
                                    Err(message) => {
                                        ui.colored_label(ERROR, message);
                                    }
                                }
                            });
                        }
                    });
            });
        });
        #[cfg(windows)]
        self.confirm_install(ctx);
    }
}

fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(18))
}

fn primary_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).strong().color(BACKGROUND)).fill(ACCENT)
}

#[cfg(windows)]
fn drive_label(drive: &Drive) -> String {
    format!(
        "{} · Disk {} · {:.1} GiB",
        drive.label,
        drive.number,
        drive.size as f64 / 1_073_741_824.0
    )
}
