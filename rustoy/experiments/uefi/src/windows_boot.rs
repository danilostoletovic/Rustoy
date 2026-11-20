//! Start the Windows boot application from staged files on the boot volume.
use alloc::vec::Vec;
use uefi::boot::{self, LoadImageSource};
use uefi::proto::BootPolicy;
use uefi::proto::device_path::{
    DevicePath,
    build::{DevicePathBuilder, media},
};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::file::{File, FileAttribute, FileMode};
use uefi::{Status, cstr16};

const APPLICATION: &uefi::CStr16 = cstr16!("\\EFI\\BOOT\\WINSETUP.EFI");

pub fn available() -> bool {
    let Ok(mut fs) = boot::get_image_file_system(boot::image_handle()) else {
        return false;
    };
    let Ok(mut root) = fs.open_volume() else {
        return false;
    };
    root.open(APPLICATION, FileMode::Read, FileAttribute::empty())
        .ok()
        .and_then(|file| file.into_regular_file())
        .is_some()
}

pub fn start() -> Result<(), Status> {
    if !available() {
        return Err(Status::NOT_FOUND);
    }
    let parent = boot::image_handle();
    let volume = boot::open_protocol_exclusive::<LoadedImage>(parent)
        .map_err(|error| error.status())?
        .device()
        .ok_or(Status::NOT_FOUND)?;
    let device =
        crate::data_disk::borrow_protocol::<DevicePath>(volume).map_err(|error| error.status())?;
    let mut bytes = Vec::new();
    let file_path = DevicePathBuilder::with_vec(&mut bytes)
        .push(&media::FilePath {
            path_name: APPLICATION,
        })
        .and_then(DevicePathBuilder::finalize)
        .map_err(|_| Status::OUT_OF_RESOURCES)?;
    let path = device
        .append_path(file_path)
        .map_err(|_| Status::UNSUPPORTED)?;
    drop(device);
    let image = boot::load_image(
        parent,
        LoadImageSource::FromDevicePath {
            device_path: &path,
            boot_policy: BootPolicy::ExactMatch,
        },
    )
    .map_err(|error| error.status())?;
    uefi::println!("Starting staged Windows boot application.");
    let result = boot::start_image(image).map_err(|error| error.status());
    // StartImage normally unloads an application when it exits.
    let _ = boot::unload_image(image);
    result
}
