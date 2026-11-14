#[cfg(not(windows))]
fn main() {
    eprintln!("This launcher requires Windows PE or Windows.");
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match windows::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Rustoy WinPE: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::{OsStr, c_void};
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    };
    use std::path::{Component, Path, PathBuf};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::GetLogicalDrives,
        System::{
            IO::DeviceIoControl,
            Ioctl::{IOCTL_STORAGE_GET_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER},
        },
    };

    #[repr(C)]
    struct StorageType {
        device: u32,
        vendor: [u8; 16],
    }
    #[repr(C)]
    struct AttachParameters {
        version: u32,
        reserved: u32,
    }

    #[link(name = "virtdisk.dll", kind = "raw-dylib")]
    unsafe extern "system" {
        fn OpenVirtualDisk(
            kind: *const StorageType,
            path: *const u16,
            access: u32,
            flags: u32,
            parameters: *const c_void,
            handle: *mut HANDLE,
        ) -> u32;
        fn AttachVirtualDisk(
            handle: HANDLE,
            security: *const c_void,
            flags: u32,
            provider_flags: u32,
            parameters: *const c_void,
            overlapped: *mut c_void,
        ) -> u32;
        fn GetVirtualDiskPhysicalPath(handle: HANDLE, bytes: *mut u32, path: *mut u16) -> u32;
    }

    fn check(status: u32) -> io::Result<()> {
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn attach(path: &Path) -> io::Result<OwnedHandle> {
        let path: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
        let kind = StorageType {
            device: 1,
            vendor: [0; 16],
        }; // ISO, select provider automatically.
        let mut handle = std::ptr::null_mut();
        // SAFETY: The path is NUL-terminated, inputs remain live during each synchronous
        // call, and its handle is transferred to OwnedHandle only after success.
        unsafe {
            check(OpenVirtualDisk(
                &kind,
                path.as_ptr(),
                0x000d_0000,
                0,
                std::ptr::null(),
                &mut handle,
            ))?;
            let handle = OwnedHandle::from_raw_handle(handle);
            // READ_ONLY. Keep the handle alive through Setup instead of requesting a permanent mount.
            let parameters = AttachParameters {
                version: 1,
                reserved: 0,
            };
            check(AttachVirtualDisk(
                handle.as_raw_handle(),
                std::ptr::null(),
                1,
                0,
                (&raw const parameters).cast(),
                std::ptr::null_mut(),
            ))?;
            Ok(handle)
        }
    }

    fn device_number(path: &Path) -> io::Result<(u32, u32)> {
        use std::os::windows::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .access_mode(0)
            .share_mode(3)
            .open(path)?;
        let mut result = STORAGE_DEVICE_NUMBER {
            DeviceType: 0,
            DeviceNumber: 0,
            PartitionNumber: 0,
        };
        let mut returned = 0;
        // SAFETY: The IOCTL writes at most the specified size into an initialized struct.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                std::ptr::null(),
                0,
                (&raw mut result).cast(),
                size_of::<STORAGE_DEVICE_NUMBER>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if returned != size_of::<STORAGE_DEVICE_NUMBER>() as u32 {
            return Err(io::Error::other("Unexpected device-number response"));
        }
        Ok((result.DeviceType, result.DeviceNumber))
    }

    fn mounted_root(handle: &OwnedHandle) -> io::Result<PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        let mut path = [0u16; 512];
        let mut len = size_of_val(&path) as u32;
        // SAFETY: len is the input buffer size in bytes and the call is synchronous.
        unsafe {
            check(GetVirtualDiskPhysicalPath(
                handle.as_raw_handle(),
                &mut len,
                path.as_mut_ptr(),
            ))?;
        }
        let end = path
            .iter()
            .position(|&word| word == 0)
            .ok_or_else(|| io::Error::other("Invalid disk path"))?;
        let physical = PathBuf::from(std::ffi::OsString::from_wide(&path[..end]));
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(expected) = device_number(&physical) {
                // SAFETY: no pointer arguments.
                let mask = unsafe { GetLogicalDrives() };
                for index in 0..26 {
                    if mask & (1 << index) == 0 {
                        continue;
                    }
                    let letter = char::from(b'A' + index);
                    if device_number(Path::new(&format!(r"\\.\{letter}:"))).ok() == Some(expected) {
                        return Ok(PathBuf::from(format!("{letter}:\\")));
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(io::Error::other(
            "The attached ISO did not receive a matching drive letter",
        ))
    }

    pub fn run() -> io::Result<()> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        let [iso] = args.as_slice() else {
            return Err(io::Error::other(
                "Usage: rustoy-winpe <absolute-local-path-to-iso>",
            ));
        };
        let iso = Path::new(iso);
        if !matches!(iso.components().next(), Some(Component::Prefix(prefix)) if
            matches!(prefix.kind(), std::path::Prefix::Disk(_)))
            || !iso.is_absolute()
            || !iso
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| ext.eq_ignore_ascii_case("iso"))
            || !File::open(iso)?.metadata()?.is_file()
        {
            return Err(io::Error::other("Expected a regular ISO on a local drive"));
        }
        let attached = attach(iso)?;
        let root = mounted_root(&attached)?;
        if !["install.wim", "install.esd", "install.swm"]
            .iter()
            .any(|name| root.join("sources").join(name).is_file())
        {
            return Err(io::Error::other(
                "The attached ISO contains no Windows install payload",
            ));
        }
        let setup = root.join("setup.exe");
        if !setup.is_file() {
            return Err(io::Error::other("The attached ISO has no setup.exe"));
        }
        let status = std::process::Command::new(setup)
            .current_dir(root)
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "Windows Setup exited with {status}"
            )));
        }
        Ok(())
    }
}
