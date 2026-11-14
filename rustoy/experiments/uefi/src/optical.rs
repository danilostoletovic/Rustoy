use alloc::{boxed::Box, vec::Vec};
use core::{cell::RefCell, ffi::c_void};
use rustoy_fs::{Error, ReadAt};
use uefi::boot::{self, LoadImageSource};
use uefi::proto::{
    BootPolicy,
    device_path::{
        DevicePath,
        build::{DevicePathBuilder, hardware, media},
    },
    media::fs::SimpleFileSystem,
};
use uefi::{Identify, Status, cstr16, guid};
use uefi_raw::Boolean;
use uefi_raw::protocol::block::{BlockIoMedia, BlockIoProtocol};

#[repr(C)]
struct Device<R> {
    protocol: BlockIoProtocol,
    media: BlockIoMedia,
    source: RefCell<R>,
    first: u64,
    length: u64,
}

struct Shared<'a, R>(&'a RefCell<R>, u64);

impl<R: ReadAt> ReadAt for Shared<'_, R> {
    fn len(&self) -> u64 {
        self.1
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        self.0
            .try_borrow_mut()
            .map_err(|_| Error::Io)?
            .read_at(offset, buffer)
    }
}

unsafe extern "efiapi" fn reset(_: *mut BlockIoProtocol, _: Boolean) -> Status {
    Status::SUCCESS
}
unsafe extern "efiapi" fn flush(_: *mut BlockIoProtocol) -> Status {
    Status::SUCCESS
}
unsafe extern "efiapi" fn write(
    _: *mut BlockIoProtocol,
    _: u32,
    _: u64,
    _: usize,
    _: *const c_void,
) -> Status {
    Status::WRITE_PROTECTED
}

unsafe extern "efiapi" fn read<R: ReadAt>(
    this: *const BlockIoProtocol,
    id: u32,
    lba: u64,
    size: usize,
    buffer: *mut c_void,
) -> Status {
    if this.is_null() {
        return Status::INVALID_PARAMETER;
    }
    if id != 1 {
        return Status::MEDIA_CHANGED;
    }
    if size == 0 {
        return Status::SUCCESS;
    }
    if buffer.is_null() || size > isize::MAX as usize {
        return Status::INVALID_PARAMETER;
    }
    // SAFETY: firmware calls this function with the live installed protocol and a writable
    // buffer spanning size bytes. The protocol is the first field of a repr(C) allocation.
    // RefCell rejects recursive reads before the output slice is borrowed.
    let device = unsafe { &*this.cast::<Device<R>>() };
    let sector = u64::from(device.media.block_size);
    if !size.is_multiple_of(sector as usize) {
        return Status::BAD_BUFFER_SIZE;
    }
    let Some(offset) = lba.checked_mul(sector) else {
        return Status::INVALID_PARAMETER;
    };
    if offset
        .checked_add(size as u64)
        .is_none_or(|end| end > device.length)
    {
        return Status::INVALID_PARAMETER;
    }
    let Ok(mut source) = device.source.try_borrow_mut() else {
        return Status::NOT_READY;
    };
    let buffer = unsafe { core::slice::from_raw_parts_mut(buffer.cast(), size) };
    match source.read_at(device.first + offset, buffer) {
        Ok(()) => Status::SUCCESS,
        Err(Error::Bounds | Error::Invalid) => Status::INVALID_PARAMETER,
        Err(_) => Status::DEVICE_ERROR,
    }
}

fn make_device<R: ReadAt>(
    source: R,
    first: u64,
    length: u64,
    sector: u32,
    partition: bool,
) -> Box<Device<R>> {
    let mut device = Box::new(Device {
        protocol: BlockIoProtocol {
            revision: 0x10000,
            media: core::ptr::null(),
            reset,
            read_blocks: read::<R>,
            write_blocks: write,
            flush_blocks: flush,
        },
        media: BlockIoMedia {
            media_id: 1,
            removable_media: (!partition).into(),
            media_present: true.into(),
            logical_partition: partition.into(),
            read_only: true.into(),
            write_caching: false.into(),
            block_size: sector,
            io_align: 1,
            last_block: length / u64::from(sector) - 1,
            lowest_aligned_lba: 0,
            logical_blocks_per_physical_block: 0,
            optimal_transfer_length_granularity: 0,
        },
        source: RefCell::new(source),
        first,
        length,
    });
    device.protocol.media = &raw const device.media;
    device
}

fn reset_on_cleanup_failure() -> ! {
    uefi::println!("Optical device cleanup failed. Restarting firmware.");
    uefi::runtime::reset(uefi::runtime::ResetType::COLD, Status::DEVICE_ERROR, None)
}

pub fn start<R: ReadAt>(mut source: R) -> Result<(), Status> {
    // This experimental path has no authenticated WinPE/ISO-data handoff.
    let mut secure = [0; 1];
    let (state, _) = uefi::runtime::get_variable(
        cstr16!("SecureBoot"),
        &uefi::runtime::VariableVendor::GLOBAL_VARIABLE,
        &mut secure,
    )
    .map_err(|_| Status::SECURITY_VIOLATION)?;
    if state != [0] {
        return Err(Status::SECURITY_VIOLATION);
    }

    let fat = rustoy_windows::efi_boot_filesystem(&mut source).map_err(|_| Status::UNSUPPORTED)?;
    let length = source.len();
    if !length.is_multiple_of(2048) {
        return Err(Status::UNSUPPORTED);
    }
    let mut bpb = [0; 512];
    source
        .read_at(fat.offset, &mut bpb)
        .map_err(|_| Status::DEVICE_ERROR)?;
    let sector = u32::from(u16::from_le_bytes([bpb[11], bpb[12]]));
    let source = RefCell::new(source);
    let optical = make_device(Shared(&source, length), 0, length, 2048, false);
    let partition = make_device(
        Shared(&source, length),
        fat.offset,
        fat.length,
        sector,
        true,
    );
    let mut parent_bytes = Vec::new();
    let vendor = hardware::Vendor {
        vendor_guid: guid!("98ec4a6c-8fb8-4697-aa13-03ec101d74e2"),
        vendor_defined_data: &[],
    };
    let parent = DevicePathBuilder::with_vec(&mut parent_bytes)
        .push(&vendor)
        .and_then(DevicePathBuilder::finalize)
        .map_err(|_| Status::OUT_OF_RESOURCES)?;
    let mut child_bytes = Vec::new();
    let cdrom = media::CdRom {
        boot_entry: 0,
        partition_start: fat.offset / 2048,
        partition_size: fat.length.div_ceil(2048),
    };
    let child = DevicePathBuilder::with_vec(&mut child_bytes)
        .push(&vendor)
        .and_then(|builder| builder.push(&cdrom))
        .and_then(DevicePathBuilder::finalize)
        .map_err(|_| Status::OUT_OF_RESOURCES)?;
    with_device(&optical, parent, |_| {
        with_device(&partition, child, |handle| {
            start_child(handle, &vendor, &cdrom)
        })
    })
}

fn with_device<R>(
    device: &Device<R>,
    path: &DevicePath,
    run: impl FnOnce(uefi::Handle) -> Result<(), Status>,
) -> Result<(), Status> {
    let path_ptr = path.as_ffi_ptr().cast::<c_void>();
    let io_ptr = (device as *const Device<R>).cast::<c_void>();
    // SAFETY: the borrowed path and device survive registration, the callback and teardown.
    // If firmware cannot unregister an interface, reset before dropping either allocation.
    let handle = unsafe { boot::install_protocol_interface(None, &DevicePath::GUID, path_ptr) }
        .map_err(|err| err.status())?;
    let installed =
        unsafe { boot::install_protocol_interface(Some(handle), &BlockIoProtocol::GUID, io_ptr) };
    let result = match installed {
        Ok(_) => {
            let result = run(handle);
            let _ = boot::disconnect_controller(handle, None, None);
            if unsafe { boot::uninstall_protocol_interface(handle, &BlockIoProtocol::GUID, io_ptr) }
                .is_err()
            {
                reset_on_cleanup_failure();
            }
            result
        }
        Err(err) => Err(err.status()),
    };
    if unsafe { boot::uninstall_protocol_interface(handle, &DevicePath::GUID, path_ptr) }.is_err() {
        reset_on_cleanup_failure();
    }
    result
}

fn start_child(
    handle: uefi::Handle,
    vendor: &hardware::Vendor<'_>,
    cdrom: &media::CdRom,
) -> Result<(), Status> {
    boot::connect_controller(handle, None, None, false).map_err(|err| err.status())?;
    let _fs =
        boot::open_protocol_exclusive::<SimpleFileSystem>(handle).map_err(|err| err.status())?;
    drop(_fs);
    let mut bytes = Vec::new();
    let path = DevicePathBuilder::with_vec(&mut bytes)
        .push(vendor)
        .and_then(|builder| builder.push(cdrom))
        .and_then(|builder| {
            builder.push(&media::FilePath {
                path_name: cstr16!("\\EFI\\BOOT\\BOOTX64.EFI"),
            })
        })
        .and_then(DevicePathBuilder::finalize)
        .map_err(|_| Status::OUT_OF_RESOURCES)?;
    let image = boot::load_image(
        boot::image_handle(),
        LoadImageSource::FromDevicePath {
            device_path: path,
            boot_policy: BootPolicy::ExactMatch,
        },
    )
    .map_err(|err| err.status())?;
    uefi::println!("RUSTOY_OPTICAL_EFI_LOADED");
    let result = boot::start_image(image).map_err(|err| err.status());
    let _ = boot::unload_image(image);
    result
}
