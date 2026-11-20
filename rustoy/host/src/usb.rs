use serde::Deserialize;
use std::{fs, io::Write, path::Path};
use wmi::{IWbemClassWrapper, Variant, WMIConnection};

const EFI: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rustoy.efi"));
const NAMESPACE: &str = r"ROOT\Microsoft\Windows\Storage";
const EFI_SYSTEM: &str = "{c12a7328-f81f-11d2-ba4b-00a0c93ec93b}";
const BASIC_DATA: &str = "{ebd0a0a2-b9e5-4433-87c0-68b6b72699c7}";
const USB: u16 = 7;
const RAW: u16 = 0;
const MBR: u16 = 1;
const GPT: u16 = 2;
const MIB: u64 = 1024 * 1024;

#[derive(Clone)]
pub struct Drive {
    pub number: u32,
    pub label: String,
    pub size: u64,
    identity: String,
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Disk {
    #[serde(rename = "__RELPATH")]
    object: String,
    path: Option<String>,
    unique_id: Option<String>,
    friendly_name: Option<String>,
    number: u32,
    size: u64,
    bus_type: u16,
    partition_style: u16,
    number_of_partitions: u32,
    is_boot: bool,
    is_system: bool,
    is_offline: bool,
    is_read_only: bool,
    is_clustered: bool,
}

impl Disk {
    fn eligible(&self) -> bool {
        self.bus_type == USB
            && self.size >= 512 * MIB
            && !self.is_boot
            && !self.is_system
            && !self.is_offline
            && !self.is_read_only
            && !self.is_clustered
            && self.unique_id.as_deref().is_some_and(valid_identity)
            && self.path.as_deref().is_some_and(valid_identity)
    }
}

fn valid_identity(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Partition {
    #[serde(rename = "__RELPATH")]
    object: String,
    disk_id: Option<String>,
    guid: Option<String>,
    gpt_type: Option<String>,
    drive_letter: Option<String>,
    offset: u64,
    size: u64,
    is_boot: bool,
    is_system: bool,
}

impl Partition {
    fn has_type(&self, kind: &str) -> bool {
        self.gpt_type.as_deref().is_some_and(|value| {
            value
                .trim_matches(['{', '}'])
                .eq_ignore_ascii_case(kind.trim_matches(['{', '}']))
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Volume {
    #[serde(rename = "__RELPATH")]
    object: String,
    path: Option<String>,
    file_system: Option<String>,
    file_system_label: Option<String>,
}

fn storage_error(error: wmi::WMIError) -> String {
    format!("Windows Storage: {error}")
}

fn storage() -> Result<WMIConnection, String> {
    WMIConnection::with_namespace_path(NAMESPACE).map_err(storage_error)
}

fn disks(storage: &WMIConnection) -> Result<Vec<Disk>, String> {
    storage
        .raw_query("SELECT * FROM MSFT_Disk WHERE BusType = 7")
        .map_err(storage_error)
}

fn partitions(storage: &WMIConnection, disk: &Disk) -> Result<Vec<Partition>, String> {
    let parts: Vec<Partition> = storage
        .raw_query(format!(
            "SELECT * FROM MSFT_Partition WHERE DiskNumber = {}",
            disk.number
        ))
        .map_err(storage_error)?;
    if parts.len() != disk.number_of_partitions as usize {
        return Err("The USB partition list changed. Refresh the drive list.".into());
    }
    if parts.iter().any(|part| part.is_boot || part.is_system) {
        return Err("The selected disk contains a boot or system partition.".into());
    }
    Ok(parts)
}

pub fn list() -> Result<Vec<Drive>, String> {
    let storage = storage()?;
    let mut result = Vec::new();
    for disk in disks(&storage)? {
        if !disk.eligible() {
            continue;
        }
        let (Some(identity), Some(path)) = (&disk.unique_id, &disk.path) else {
            continue;
        };
        if partitions(&storage, &disk).is_ok() {
            result.push(Drive {
                number: disk.number,
                label: disk
                    .friendly_name
                    .unwrap_or_else(|| "USB drive".into())
                    .replace(char::is_control, " "),
                size: disk.size,
                identity: identity.clone(),
                path: path.clone(),
            });
        }
    }
    Ok(result)
}

fn target(storage: &WMIConnection, drive: &Drive) -> Result<(Disk, Vec<Partition>), String> {
    let candidates = disks(storage)?;
    let mut matches = candidates.into_iter().filter(|disk| {
        disk.unique_id.as_deref() == Some(&drive.identity)
            && disk.path.as_deref() == Some(&drive.path)
    });
    let disk = matches
        .next()
        .ok_or("The selected USB drive is unavailable. Refresh the drive list.")?;
    if matches.next().is_some()
        || disk.number != drive.number
        || disk.size != drive.size
        || !disk.eligible()
    {
        return Err("The selected USB drive changed. Refresh the drive list.".into());
    }
    let parts = partitions(storage, &disk)?;
    Ok((disk, parts))
}

fn call(
    storage: &WMIConnection,
    class: &str,
    object: &str,
    method: &str,
    args: &[(&str, Variant)],
) -> Result<(), String> {
    fn invoke(
        storage: &WMIConnection,
        class: &str,
        object: &str,
        method: &str,
        args: &[(&str, Variant)],
    ) -> wmi::WMIResult<Option<IWbemClassWrapper>> {
        let mut input = None;
        if !args.is_empty()
            && let Some(parameters) = storage.get_object(class)?.get_method(method)?
        {
            let parameters = parameters.spawn_instance()?;
            for (name, value) in args {
                parameters.put_property(name, value.clone())?;
            }
            input = Some(parameters);
        }
        if !args.is_empty() && input.is_none() {
            return Ok(None);
        }
        storage.exec_method(object, method, input.as_ref())
    }

    let output = invoke(storage, class, object, method, args)
        .map_err(|error| format!("{method}: {}", storage_error(error)))?
        .ok_or_else(|| format!("Windows Storage returned no result for {method}"))?;
    let result = output.get_property("ReturnValue").map_err(storage_error)?;
    if result != Variant::UI4(0) {
        return Err(format!("Windows Storage {method} failed ({result:?})."));
    }
    Ok(())
}

fn empty_gpt(storage: &WMIConnection, drive: &Drive) -> Result<(), String> {
    let (mut disk, mut parts) = target(storage, drive)?;
    if disk.partition_style == MBR {
        call(
            storage,
            "MSFT_Disk",
            &disk.object,
            "Clear",
            &[("RemoveData", true.into()), ("RemoveOEM", true.into())],
        )?;
        (disk, parts) = target(storage, drive)?;
    }
    if disk.partition_style == RAW && parts.is_empty() {
        call(
            storage,
            "MSFT_Disk",
            &disk.object,
            "Initialize",
            &[("PartitionStyle", GPT.into())],
        )?;
        (disk, parts) = target(storage, drive)?;
    }
    if disk.partition_style != GPT {
        return Err("Windows did not report GPT on the selected USB drive.".into());
    }
    while let Some(part) = parts.first() {
        call(storage, "MSFT_Partition", &part.object, "DeleteObject", &[])?;
        let (current, remaining) = target(storage, drive)?;
        if current.partition_style != GPT
            || remaining.len() + 1 != parts.len()
            || remaining.iter().any(|found| {
                !parts[1..].iter().any(|before| {
                    found.guid == before.guid
                        && found.offset == before.offset
                        && found.size == before.size
                })
            })
        {
            return Err("The selected USB partition list changed during removal.".into());
        }
        parts = remaining;
    }
    Ok(())
}

fn create_partition(
    storage: &WMIConnection,
    drive: &Drive,
    existing: &[&str],
    size: Option<u64>,
) -> Result<String, String> {
    let (disk, parts) = target(storage, drive)?;
    if disk.partition_style != GPT
        || parts.len() != existing.len()
        || parts.iter().any(|part| {
            !part.has_type(BASIC_DATA)
                || !part
                    .guid
                    .as_deref()
                    .is_some_and(|guid| existing.contains(&guid))
        })
    {
        return Err("The USB layout changed before partition creation.".into());
    }
    let size_arg = match size {
        Some(bytes) => ("Size", bytes.into()),
        None => ("UseMaximumSize", true.into()),
    };
    call(
        storage,
        "MSFT_Disk",
        &disk.object,
        "CreatePartition",
        &[
            size_arg,
            ("AssignDriveLetter", true.into()),
            ("GptType", BASIC_DATA.into()),
        ],
    )?;
    let (_, parts) = target(storage, drive)?;
    let mut new = parts.iter().filter(|part| {
        part.guid
            .as_deref()
            .is_some_and(|guid| !guid.is_empty() && !existing.contains(&guid))
    });
    if let Some(part) = new.next()
        && new.next().is_none()
        && parts.len() == existing.len() + 1
        && part.has_type(BASIC_DATA)
        && size.is_none_or(|bytes| part.size == bytes)
        && let Some(guid) = &part.guid
    {
        return Ok(guid.clone());
    }
    Err("Windows did not confirm creation of the expected USB partition.".into())
}

fn partition(storage: &WMIConnection, drive: &Drive, guid: &str) -> Result<Partition, String> {
    let (_, parts) = target(storage, drive)?;
    let mut matching = parts
        .into_iter()
        .filter(|part| part.guid.as_deref() == Some(guid));
    if let Some(part) = matching.next()
        && matching.next().is_none()
    {
        return Ok(part);
    }
    Err("A new USB partition changed or disappeared.".into())
}

fn volume(storage: &WMIConnection, part: &Partition) -> Result<Volume, String> {
    let volumes: Vec<Volume> = storage
        .raw_query(format!(
            "ASSOCIATORS OF {{{}}} WHERE AssocClass = MSFT_PartitionToVolume ResultClass = MSFT_Volume",
            part.object
        ))
        .map_err(storage_error)?;
    let [volume]: [Volume; 1] = volumes
        .try_into()
        .map_err(|_| "Windows did not report one volume for the USB partition.")?;
    Ok(volume)
}

fn format_volume(
    storage: &WMIConnection,
    drive: &Drive,
    guid: &str,
    filesystem: &str,
    label: &str,
) -> Result<Volume, String> {
    let part = partition(storage, drive, guid)?;
    if !part.has_type(BASIC_DATA) {
        return Err("The new USB partition type changed before formatting.".into());
    }
    drive_root(&part)?;
    let current = volume(storage, &part)?;
    call(
        storage,
        "MSFT_Volume",
        &current.object,
        "Format",
        &[
            ("FileSystem", filesystem.into()),
            ("FileSystemLabel", label.into()),
            ("Full", false.into()),
        ],
    )?;
    let formatted = volume(storage, &partition(storage, drive, guid)?)?;
    if formatted
        .file_system
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(filesystem))
        && formatted.file_system_label.as_deref() == Some(label)
    {
        return Ok(formatted);
    }
    Err(format!("Windows did not confirm the {label} format."))
}

fn drive_root(part: &Partition) -> Result<String, String> {
    let letter = part
        .drive_letter
        .as_deref()
        .filter(|letter| letter.len() == 1 && letter.as_bytes()[0].is_ascii_alphabetic())
        .ok_or("Windows did not assign a USB drive letter.")?;
    Ok(format!("{letter}:\\"))
}

fn volume_path(volume: &Volume) -> Result<&Path, String> {
    let path = volume
        .path
        .as_deref()
        .ok_or("Windows did not report a USB volume path.")?;
    let root = path
        .strip_prefix(r"\\?\Volume{")
        .and_then(|path| path.strip_suffix("}\\"));
    if root.is_some_and(|guid| {
        guid.len() == 36
            && guid.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
    }) {
        return Ok(Path::new(path));
    }
    Err("Windows returned an invalid USB volume path.".into())
}

fn install_to(storage: &WMIConnection, drive: &Drive) -> Result<(), String> {
    empty_gpt(storage, drive)?;
    let esp = create_partition(storage, drive, &[], Some(256 * MIB))?;
    let data = create_partition(storage, drive, &[&esp], None)?;
    format_volume(storage, drive, &data, "exFAT", "RUSTOY")?;
    let boot_volume = format_volume(storage, drive, &esp, "FAT32", "RUSTOYEFI")?;
    let boot = volume_path(&boot_volume)?.join(r"EFI\BOOT");
    fs::create_dir_all(&boot).map_err(|error| error.to_string())?;
    let path = boot.join("BOOTX64.EFI");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| error.to_string())?;
    file.write_all(EFI)
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())?;
    drop(file);
    if fs::read(path).map_err(|error| error.to_string())? != EFI {
        return Err("EFI loader read-back verification failed.".into());
    }

    let part = partition(storage, drive, &esp)?;
    let root = drive_root(&part)?;
    call(
        storage,
        "MSFT_Partition",
        &part.object,
        "RemoveAccessPath",
        &[("AccessPath", root.into())],
    )?;
    let part = partition(storage, drive, &esp)?;
    let disk_id = part
        .disk_id
        .as_deref()
        .filter(|id| valid_identity(id))
        .ok_or("Windows did not report the disk identity for the EFI partition.")?;
    call(
        storage,
        "PS_StorageCmdlets",
        "PS_StorageCmdlets",
        "SetPartition",
        &[
            ("DiskId", disk_id.into()),
            ("Offset", part.offset.into()),
            ("GptType", EFI_SYSTEM.into()),
        ],
    )?;
    let (disk, parts) = target(storage, drive)?;
    let data_part = partition(storage, drive, &data)?;
    let data_volume = volume(storage, &data_part)?;
    if disk.partition_style != GPT
        || parts.len() != 2
        || !parts
            .iter()
            .any(|part| part.guid.as_deref() == Some(&esp) && part.has_type(EFI_SYSTEM))
        || !data_part.has_type(BASIC_DATA)
        || drive_root(&data_part).is_err()
        || !data_volume
            .file_system
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("exFAT"))
    {
        return Err("The final USB partition layout could not be verified.".into());
    }
    Ok(())
}

pub fn install(drive: Drive) -> Result<String, String> {
    if EFI.is_empty() {
        return Err("This build has no EFI payload. Rebuild with RUSTOY_EFI_IMAGE set.".into());
    }
    if !is_admin() {
        return Err(
            "Close Rustoy, then right-click the executable and choose Run as administrator.".into(),
        );
    }
    install_to(&storage()?, &drive)
        .map_err(|error| format!("USB setup did not finish. {error}"))?;
    Ok(format!(
        "Installed on {} (Disk {}). Copy your ISOs to the root of the RUSTOY volume.",
        drive.label, drive.number
    ))
}

#[allow(unsafe_code)]
fn is_admin() -> bool {
    // SAFETY: IsUserAnAdmin takes no pointers or handles.
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}
