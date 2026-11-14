#![cfg_attr(windows, windows_subsystem = "windows")]
#![forbid(unsafe_code)]

#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    eprintln!("Rustoy Desktop is available on Windows and Linux.");
}

#[cfg(any(windows, target_os = "linux"))]
#[path = "rustoy-desktop/desktop.rs"]
mod desktop;

#[cfg(any(windows, target_os = "linux"))]
fn main() -> eframe::Result {
    desktop::run()
}
