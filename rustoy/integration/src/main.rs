//! Disposable OVMF smoke tests with read-only USB media.
#![forbid(unsafe_code)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
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
    let boot_debian = matches!(isos, [flag] if flag == Path::new("--boot-debian"));
    let expected = if boot_debian {
        Some(1)
    } else if let [flag, count] = isos
        && flag.as_os_str() == "--count"
    {
        Some(
            count
                .to_str()
                .and_then(|text| text.parse::<usize>().ok())
                .ok_or_else(|| io::Error::other("invalid discovery count"))?,
        )
    } else {
        None
    };
    let (media, expected) = if let Some(count) = expected {
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
    let mut command = Command::new(qemu);
    if boot_debian {
        command
            .args([
                "-machine",
                "q35,accel=tcg,smm=on",
                "-smp",
                "2",
                "-m",
                "2048",
            ])
            .args(["-global", "driver=cfi.pflash01,property=secure,value=on"])
            .args(["-monitor", "stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
    } else {
        command.args(["-machine", "q35,accel=tcg", "-m", "256", "-monitor", "none"]);
    }
    let mut child = command
        .args(["-display", "none", "-no-reboot", "-nic", "none"])
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
    let deadline = Instant::now() + Duration::from_secs(if boot_debian { 240 } else { 45 });
    let discovery_marker = format!("RUSTOY_ISO_COUNT={expected}");
    let mut monitor = child.stdin.take();
    let mut sent = false;
    let result = loop {
        if let Ok(log) = fs::read_to_string(&serial) {
            let log = strip_terminal_sequences(&log);
            let discovered = log.contains("RUSTOY_EFI_STARTED")
                && log.lines().any(|line| line.trim() == discovery_marker);
            if discovered {
                if !boot_debian {
                    break Ok(());
                }
                if !sent {
                    let Some(stdin) = &mut monitor else {
                        break Err(io::Error::other("QEMU monitor input is unavailable"));
                    };
                    if let Err(err) = stdin
                        .write_all(b"sendkey ret\n")
                        .and_then(|()| stdin.flush())
                    {
                        break Err(err);
                    }
                    sent = true;
                }
                if log.contains("Starting Debian Live EFI kernel.")
                    && log.contains("/run/live/rootfs/")
                    && (log.contains("Reached target multi-user.target")
                        || log.contains("debian login:"))
                {
                    break Ok(());
                }
                if let Some(failure) = log
                    .lines()
                    .find(|line| line.contains("Debian Live boot failed:"))
                {
                    break Err(io::Error::other(failure));
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break Err(io::Error::other(format!("QEMU exited with {status}"))),
            Err(err) => break Err(err),
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            break Err(io::Error::other(if boot_debian {
                "timed out waiting for Debian Live to reach userspace"
            } else {
                "timed out waiting for the Rustoy startup and discovery markers"
            }));
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
        eprintln!(
            "       rustoy-integration <qemu> <ovmf-secure-code> <ovmf-vars> <gpt.img> <new-output-dir> --boot-debian"
        );
        return ExitCode::from(2);
    };
    match smoke(qemu, code, vars, app, output, isos) {
        Ok(()) => {
            println!(
                "OVMF smoke test passed. Transcript at {}",
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
