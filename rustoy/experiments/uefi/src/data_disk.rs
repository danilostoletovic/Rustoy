use alloc::vec;
use alloc::vec::Vec;
use rustoy_fs::{Error, ReadAt, exfat, gpt};
use uefi::Handle;
use uefi::boot::{self, ScopedProtocol};
use uefi::proto::ProtocolPointer;
use uefi::proto::device_path::{DevicePath, media};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;

const TRANSFER: usize = 32 * 1024;
const ALIGNMENT: usize = 4096;

fn borrow_protocol<P: ProtocolPointer + ?Sized>(handle: Handle) -> uefi::Result<ScopedProtocol<P>> {
    // SAFETY: The synchronous read path never connects or disconnects controllers,
    // invokes child images, or retains these interfaces across menu input.
    unsafe {
        boot::open_protocol(
            boot::OpenProtocolParams {
                handle,
                agent: boot::image_handle(),
                controller: None,
            },
            boot::OpenProtocolAttributes::GetProtocol,
        )
    }
}

pub struct Disk {
    protocol: ScopedProtocol<BlockIO>,
    media_id: u32,
    length: u64,
    sector: u64,
    buffer: Vec<u8>,
}

impl ReadAt for Disk {
    fn len(&self) -> u64 {
        self.length
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        if offset
            .checked_add(buffer.len() as u64)
            .ok_or(Error::Bounds)?
            > self.length
        {
            return Err(Error::Bounds);
        }
        let alignment = self.buffer.as_ptr().align_offset(ALIGNMENT);
        let bounce = &mut self.buffer[alignment..alignment + TRANSFER];
        let sector = self.sector as usize;
        let mut offset = offset;
        let mut rest = buffer;
        while !rest.is_empty() {
            let start = (offset % self.sector) as usize;
            let count = rest.len().min(TRANSFER - start);
            let transfer = (start + count).next_multiple_of(sector);
            self.protocol
                .read_blocks(self.media_id, offset / self.sector, &mut bounce[..transfer])
                .map_err(|_| Error::Io)?;
            rest[..count].copy_from_slice(&bounce[start..start + count]);
            rest = &mut rest[count..];
            offset += count as u64;
        }
        Ok(())
    }
}

pub fn open() -> Result<Option<exfat::Volume<Disk>>, Error> {
    let loaded = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
        .map_err(|_| Error::Io)?;
    let partition = loaded.device().ok_or(Error::NotFound)?;
    let path = borrow_protocol::<DevicePath>(partition).map_err(|_| Error::Io)?;
    let mut nodes: Vec<_> = path.node_iter().collect();
    let Some(last) = nodes.pop() else {
        return Ok(None);
    };
    let Ok(hd) = <&media::HardDrive>::try_from(last) else {
        return Ok(None);
    };
    let media::PartitionSignature::Guid(esp_id) = hd.partition_signature() else {
        return Ok(None);
    };
    for handle in boot::find_handles::<BlockIO>().map_err(|_| Error::Io)? {
        if handle == partition {
            continue;
        }
        let Ok(candidate) = borrow_protocol::<DevicePath>(handle) else {
            continue;
        };
        if !candidate.node_iter().eq(nodes.iter().copied()) {
            continue;
        }
        let block = borrow_protocol::<BlockIO>(handle).map_err(|_| Error::Io)?;
        let media = block.media();
        if media.is_logical_partition() || !media.is_media_present() {
            continue;
        }
        let sector = u64::from(media.block_size());
        if !matches!(sector, 512 | 4096) || u64::from(media.io_align()) > ALIGNMENT as u64 {
            return Err(Error::Unsupported);
        }
        let length = media
            .last_block()
            .checked_add(1)
            .and_then(|blocks| blocks.checked_mul(sector))
            .ok_or(Error::Bounds)?;
        let media_id = media.media_id();
        let mut disk = Disk {
            protocol: block,
            media_id,
            length,
            sector,
            buffer: vec![0; TRANSFER + ALIGNMENT - 1],
        };
        let mut table = vec![0; gpt::MAX_TABLE_BYTES];
        let mut slots = vec![
            gpt::Partition {
                slot: 0,
                kind: [0; 16],
                id: [0; 16],
                first_lba: 0,
                last_lba: 0
            };
            gpt::MAX_TABLE_BYTES / 128
        ];
        let (_, partitions) = gpt::read_layout(&mut disk, sector as usize, &mut table, &mut slots)?;
        if !partitions
            .iter()
            .any(|part| part.kind == gpt::EFI_SYSTEM && uefi::Guid::from_bytes(part.id) == esp_id)
        {
            return Err(Error::Invalid);
        }
        let mut data = partitions
            .iter()
            .filter(|part| part.kind == gpt::BASIC_DATA);
        let part = data.next().ok_or(Error::NotFound)?;
        if data.next().is_some() {
            return Err(Error::Unsupported);
        }
        return exfat::Volume::open(
            disk,
            part.first_lba * sector,
            (part.last_lba - part.first_lba + 1) * sector,
        )
        .map(Some);
    }
    Err(Error::NotFound)
}
