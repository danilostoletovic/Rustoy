//! Disposable OVMF smoke test. The exported FAT directory is read-only.
#![forbid(unsafe_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

fn smoke(
    qemu: &Path,
    code: &Path,
    vars: &Path,
    app: &Path,
    output: &Path,
    isos: &[PathBuf],
) -> io::Result<()> {
    fs::create_dir(output)?;
    let output = output.canonicalize()?;
    let (media, expected) = if let [flag, count] = isos
        && flag.as_os_str() == "--count"
    {
        let count = count
            .to_str()
            .and_then(|text| text.parse::<usize>().ok())
            .ok_or_else(|| io::Error::other("invalid discovery count"))?;
        if !fs::metadata(app)?.is_file() {
            return Err(io::Error::other("expected a regular GPT image"));
        }
        (qemu_path(&app.canonicalize()?), count)
    } else {
        let esp = output.join("esp");
        let boot = esp.join("EFI/BOOT");
        fs::create_dir_all(&boot)?;
        fs::copy(app, boot.join("BOOTX64.EFI"))?;
        for (index, iso) in isos.iter().enumerate() {
            fs::copy(iso, esp.join(format!("fixture-{index}.iso")))?;
        }
        (format!("fat:{}", qemu_path(&esp)), isos.len())
    };
    let vars_copy = output.join("vars.fd");
    fs::copy(vars, &vars_copy)?;
    let serial = output.join("serial.log");
    let mut child = Command::new(qemu)
        .args(["-machine", "q35,accel=tcg", "-m", "256", "-display", "none"])
        .args(["-monitor", "none", "-no-reboot", "-nic", "none"])
        .args([
            "-drive",
            &format!("if=pflash,format=raw,readonly=on,file={}", qemu_path(code)),
        ])
        .args([
            "-drive",
            &format!("if=pflash,format=raw,file={}", qemu_path(&vars_copy)),
        ])
        .args(["-device", "qemu-xhci,id=usb"])
        .args([
            "-drive",
            &format!("if=none,id=stick,format=raw,readonly=on,file={media}"),
        ])
        .args(["-device", "usb-storage,bus=usb.0,drive=stick,bootindex=1"])
        .args(["-serial", &format!("file:{}", serial.display())])
        .stderr(fs::File::create(output.join("qemu.log"))?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let discovery_marker = format!("RUSTOY_ISO_COUNT={expected}");
    let result = loop {
        if fs::read_to_string(&serial).is_ok_and(|log| {
            let log = strip_terminal_sequences(&log);
            log.contains("RUSTOY_EFI_STARTED")
                && log.lines().any(|line| line.trim() == discovery_marker)
        }) {
            break Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Err(io::Error::other(format!("QEMU exited with {status}"))),
            Err(err) => break Err(err),
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            break Err(io::Error::other(
                "timed out waiting for the Rustoy startup and discovery markers",
            ));
        }
        thread::sleep(Duration::from_millis(100));
    };
    let _ = child.kill();
    child.wait()?;
    result
}

fn strip_terminal_sequences(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.next_if_eq(&'[').is_some() {
            for parameter in chars.by_ref() {
                if ('@'..='~').contains(&parameter) {
                    break;
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}

fn qemu_path(path: &Path) -> String {
    path.display().to_string().replace(',', ",,")
}

fn main() -> ExitCode {
    let paths: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [qemu, code, vars, app, output, isos @ ..] = paths.as_slice() else {
        eprintln!(
            "Usage: rustoy-integration <qemu> <ovmf-code> <ovmf-vars> <efi-app> <new-output-dir> [iso ...]"
        );
        eprintln!(
            "       rustoy-integration <qemu> <ovmf-code> <ovmf-vars> <gpt.img> <new-output-dir> --count <n>"
        );
        return ExitCode::from(2);
    };
    match smoke(qemu, code, vars, app, output, isos) {
        Ok(()) => {
            println!(
                "Observed RUSTOY_EFI_STARTED in {}",
                output.join("serial.log").display()
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("OVMF smoke test failed: {err}");
            ExitCode::FAILURE
        }
    }
}
