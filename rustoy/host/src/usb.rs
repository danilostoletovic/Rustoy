use std::{
    io::Write,
    os::windows::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

const EFI: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rustoy.efi"));
const DISCOVER: &str = include_str!("usb/discover.ps1");
const INSTALL: &str = include_str!("usb/install.ps1");

#[derive(Clone)]
pub struct Drive {
    pub number: u32,
    pub label: String,
    pub size: u64,
    identity: String,
    path: String,
}

fn powershell(script: &str, vars: &[(&str, &std::ffi::OsStr)]) -> Result<String, String> {
    let windows = std::env::var_os("SystemRoot").ok_or("Windows directory is unavailable")?;
    let exe = Path::new(&windows).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    // Windows PowerShell -Command - needs a blank line after a multiline
    // command. Closing stdin immediately after the final brace can skip it.
    let script = format!(
        "& {{\n$ErrorActionPreference = 'Stop'\n[Console]::OutputEncoding = [Text.UTF8Encoding]::new()\ntry {{\n{script}\n}} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}\n}}\n\n"
    );
    let mut child = Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
        .envs(vars.iter().copied())
        .creation_flags(0x0800_0000)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .map_err(|err| err.to_string())?;
    let result = child.wait_with_output().map_err(|err| err.to_string())?;
    let output = String::from_utf8_lossy(&result.stdout).trim().to_owned();
    let error = String::from_utf8_lossy(&result.stderr);
    if !result.status.success() || !error.trim().is_empty() {
        return Err(if error.trim().is_empty() {
            format!("Windows Storage exited with {}", result.status)
        } else {
            error.trim().to_owned()
        });
    }
    Ok(output)
}

pub fn list() -> Result<Vec<Drive>, String> {
    let output = powershell(DISCOVER, &[])?;
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            let [number, size, identity, path, label] = fields.as_slice() else {
                return Err("Unexpected Windows Storage output".into());
            };
            Ok(Drive {
                number: number.parse().map_err(|_| "Invalid Windows disk number")?,
                size: size.parse().map_err(|_| "Invalid Windows disk size")?,
                identity: (*identity).into(),
                path: (*path).into(),
                label: (*label).into(),
            })
        })
        .collect()
}

pub fn install(drive: Drive) -> Result<String, String> {
    use std::ffi::OsStr;
    if EFI.is_empty() {
        return Err("This build has no EFI payload. Rebuild with RUSTOY_EFI_IMAGE set.".into());
    }
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|err| err.to_string())?;
    let token: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let payload = std::env::temp_dir().join(format!("rustoy-{token}.efi"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&payload)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(EFI).map_err(|err| err.to_string())?;
        file.sync_all().map_err(|err| err.to_string())?;
        drop(file);
        let output = powershell(
            INSTALL,
            &[
                ("RUSTOY_NUMBER", OsStr::new(&drive.number.to_string())),
                ("RUSTOY_SIZE", OsStr::new(&drive.size.to_string())),
                ("RUSTOY_ID", OsStr::new(&drive.identity)),
                ("RUSTOY_PATH", OsStr::new(&drive.path)),
                ("RUSTOY_EFI", payload.as_os_str()),
            ],
        )?;
        if !output.lines().any(|line| line == "RUSTOY_INSTALL_VERIFIED") {
            return Err(format!(
                "Windows did not confirm installation. Inspect the selected drive before retrying.\n{output}"
            ));
        }
        Ok(format!(
            "Installed on {} (Disk {}). Copy your ISOs to the root of the RUSTOY volume.",
            drive.label, drive.number
        ))
    })();
    let _ = std::fs::remove_file(payload);
    result
}
