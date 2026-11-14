use alloc::vec::Vec;
use core::ffi::c_void;
use rustoy_fs::ReadAt;
use rustoy_iso::Iso;
use rustoy_linux::DebianLiveAssets;
use uefi::boot::{self, LoadImageSource};
use uefi::proto::{
    device_path::{
        DevicePath,
        build::{DevicePathBuilder, media},
    },
    loaded_image::LoadedImage,
};
use uefi::{Identify, Status, cstr16, guid};
use uefi_raw::{
    Boolean,
    protocol::{device_path::DevicePathProtocol, media::LoadFile2Protocol},
};

#[repr(C)]
struct Initrd {
    protocol: LoadFile2Protocol,
    bytes: Vec<u8>,
}

unsafe extern "efiapi" fn load_initrd(
    this: *mut LoadFile2Protocol,
    path: *const DevicePathProtocol,
    boot_policy: Boolean,
    size: *mut usize,
    buffer: *mut c_void,
) -> Status {
    if this.is_null() || path.is_null() || size.is_null() {
        return Status::INVALID_PARAMETER;
    }
    if bool::from(boot_policy) {
        return Status::UNSUPPORTED;
    }
    // SAFETY: UEFI supplies protocol and size pointers and a remaining device-path node.
    // Initrd stays allocated until the interfaces have been removed.
    let (initrd, path, size) = unsafe { (&*this.cast::<Initrd>(), &*path, &mut *size) };
    if path.major_type.0 != 0x7f || path.sub_type.0 != 0xff || path.length != [4, 0] {
        return Status::NOT_FOUND;
    }
    if buffer.is_null() || *size < initrd.bytes.len() {
        *size = initrd.bytes.len();
        return Status::BUFFER_TOO_SMALL;
    }
    // SAFETY: caller supplies a writable, non-overlapping buffer of at least *size bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(initrd.bytes.as_ptr(), buffer.cast(), initrd.bytes.len())
    };
    *size = initrd.bytes.len();
    Status::SUCCESS
}

fn require_experimental_boot() -> Result<(), Status> {
    let mut secure = [0; 1];
    let (state, _) = uefi::runtime::get_variable(
        cstr16!("SecureBoot"),
        &uefi::runtime::VariableVendor::GLOBAL_VARIABLE,
        &mut secure,
    )
    .map_err(|_| Status::SECURITY_VIOLATION)?;
    if state == [0] {
        Ok(())
    } else {
        Err(Status::SECURITY_VIOLATION)
    }
}

pub fn start(source: impl ReadAt, name: &str) -> Result<(), Status> {
    require_experimental_boot()?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(Status::INVALID_PARAMETER);
    }
    let mut iso = Iso::open(source).map_err(|_| Status::UNSUPPORTED)?;
    let assets = DebianLiveAssets::locate(&mut iso).map_err(|_| Status::UNSUPPORTED)?;
    if assets.kernel.length > 64 * 1024 * 1024 || assets.initrd.length > 256 * 1024 * 1024 {
        return Err(Status::OUT_OF_RESOURCES);
    }
    let mut kernel = boot_buffer(assets.kernel.length as usize)?;
    let mut initrd = Initrd {
        protocol: LoadFile2Protocol {
            load_file: load_initrd,
        },
        bytes: boot_buffer(assets.initrd.length as usize)?,
    };
    iso.read(assets.kernel, 0, &mut kernel)
        .map_err(|_| Status::DEVICE_ERROR)?;
    iso.read(assets.initrd, 0, &mut initrd.bytes)
        .map_err(|_| Status::DEVICE_ERROR)?;
    let options: Vec<u16> = alloc::format!(
        "boot=live components findiso=/{name} noeject console=tty0 console=ttyS0,115200n8"
    )
    .encode_utf16()
    .chain([0])
    .collect();

    let mut path_bytes = Vec::new();
    let path = DevicePathBuilder::with_vec(&mut path_bytes)
        .push(&media::Vendor {
            vendor_guid: guid!("5568e427-68fc-4f3d-ac74-ca555231cc68"),
            vendor_defined_data: &[],
        })
        .and_then(DevicePathBuilder::finalize)
        .map_err(|_| Status::OUT_OF_RESOURCES)?;
    let mut remaining = path;
    if boot::locate_device_path::<uefi::proto::media::load_file::LoadFile2>(&mut remaining).is_ok()
        && remaining.node_iter().next().is_none()
    {
        return Err(Status::ALREADY_STARTED);
    }
    let path_ptr = path.as_ffi_ptr().cast::<c_void>();
    let protocol_ptr = (&raw mut initrd.protocol).cast::<c_void>();
    // SAFETY: path and initrd live at stable addresses throughout registration and
    // child execution. Reset on teardown failure to avoid retained dangling pointers.
    let handle = unsafe { boot::install_protocol_interface(None, &DevicePath::GUID, path_ptr) }
        .map_err(|err| err.status())?;
    let result = match unsafe {
        boot::install_protocol_interface(Some(handle), &LoadFile2Protocol::GUID, protocol_ptr)
    } {
        Ok(_) => {
            let result = start_kernel(&kernel, &options);
            if unsafe {
                boot::uninstall_protocol_interface(handle, &LoadFile2Protocol::GUID, protocol_ptr)
            }
            .is_err()
            {
                cleanup_failure();
            }
            result
        }
        Err(err) => Err(err.status()),
    };
    if unsafe { boot::uninstall_protocol_interface(handle, &DevicePath::GUID, path_ptr) }.is_err() {
        cleanup_failure();
    }
    result
}

fn boot_buffer(length: usize) -> Result<Vec<u8>, Status> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(length)
        .map_err(|_| Status::OUT_OF_RESOURCES)?;
    buffer.resize(length, 0);
    Ok(buffer)
}

fn start_kernel(kernel: &[u8], options: &[u16]) -> Result<(), Status> {
    let image = boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromBuffer {
            buffer: kernel,
            file_path: None,
        },
    )
    .map_err(|err| err.status())?;
    let result = (|| {
        let mut loaded =
            boot::open_protocol_exclusive::<LoadedImage>(image).map_err(|err| err.status())?;
        // SAFETY: the UTF-16 buffer is immutable and remains live through StartImage
        // and, if the child returns, through unloading or clearing its options.
        unsafe { loaded.set_load_options(options.as_ptr().cast(), size_of_val(options) as u32) };
        drop(loaded);
        uefi::println!("Starting Debian Live EFI kernel.");
        boot::start_image(image).map_err(|err| err.status())
    })();
    if let Err(err) = boot::unload_image(image) {
        if err.status() != Status::INVALID_PARAMETER {
            cleanup_failure();
        }
    }
    result
}

fn cleanup_failure() -> ! {
    uefi::runtime::reset(uefi::runtime::ResetType::COLD, Status::DEVICE_ERROR, None)
}
