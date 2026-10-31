//! A read-only FAT12/16/32 file system reader, plus partition table
//! parsing (MBR and GPT). FAT is what USB sticks and the UEFI boot
//! partition use.

use crate::usb::storage::UsbStorage;
use alloc::string::String;
use alloc::vec::Vec;

pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u32,
    cluster: u32,
}

#[derive(PartialEq)]
enum Kind {
    Fat12,
    Fat16,
    Fat32,
}

pub struct FileSystem {
    disk: UsbStorage,
    l: Layout,
}

/// Where everything is on the partition, from its boot sector.
struct Layout {
    /// Block where the partition starts.
    start: u64,
    kind: Kind,
    sector_size: u32,
    sectors_per_cluster: u32,
    fat_start: u64,
    root_dir_start: u64,
    root_dir_sectors: u32,
    data_start: u64,
    root_cluster: u32,
}

impl FileSystem {
    /// Find the first FAT partition on the disk and open it.
    pub fn open(mut disk: UsbStorage) -> Result<Self, &'static str> {
        if disk.block_size != 512 {
            return Err("only 512-byte sectors are supported so far");
        }
        let mut sector = [0u8; 512];
        let mut candidates: Vec<u64> = Vec::new();

        // GPT: header in block 1, partition entries after it.
        disk.read_blocks(1, 1, &mut sector)?;
        if &sector[0..8] == b"EFI PART" {
            let entries_lba = u64::from_le_bytes(sector[72..80].try_into().unwrap());
            let count = u32::from_le_bytes(sector[80..84].try_into().unwrap()).min(128);
            let entry_size = u32::from_le_bytes(sector[84..88].try_into().unwrap()) as usize;
            let blocks = (count as usize * entry_size).div_ceil(512);
            let mut table = alloc::vec![0u8; blocks * 512];
            disk.read_blocks(entries_lba, blocks, &mut table)?;
            for entry in table.chunks(entry_size).take(count as usize) {
                if entry[0..16].iter().any(|&b| b != 0) {
                    candidates.push(u64::from_le_bytes(entry[32..40].try_into().unwrap()));
                }
            }
        } else {
            // MBR: four 16-byte entries at offset 446.
            disk.read_blocks(0, 1, &mut sector)?;
            if sector[510..512] == [0x55, 0xAA] {
                for entry in sector[446..510].chunks(16) {
                    let start = u32::from_le_bytes(entry[8..12].try_into().unwrap());
                    if entry[4] != 0 && start != 0 {
                        candidates.push(start as u64);
                    }
                }
            }
            // A stick may also have no partition table at all.
            candidates.push(0);
        }

        for start in candidates {
            disk.read_blocks(start, 1, &mut sector)?;
            if let Some(fs) = Self::parse_boot_sector(&sector, start) {
                return Ok(Self { disk, l: fs });
            }
        }
        Err("no FAT partition found")
    }

    /// Read the FAT "boot sector" (BPB), which describes the layout.
    fn parse_boot_sector(b: &[u8; 512], start: u64) -> Option<Layout> {
        let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as u32;
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        if b[510..512] != [0x55, 0xAA] {
            return None;
        }
        let sector_size = u16_at(11);
        let sectors_per_cluster = b[13] as u32;
        let reserved = u16_at(14);
        let fats = b[16] as u32;
        let root_entries = u16_at(17);
        let total = if u16_at(19) != 0 { u16_at(19) } else { u32_at(32) };
        let fat_size = if u16_at(22) != 0 { u16_at(22) } else { u32_at(36) };
        if sector_size != 512 || sectors_per_cluster == 0 || fats == 0 || fat_size == 0 {
            return None;
        }
        let root_dir_sectors = (root_entries * 32).div_ceil(sector_size);
        let data_sectors = total.checked_sub(reserved + fats * fat_size + root_dir_sectors)?;
        let clusters = data_sectors / sectors_per_cluster;
        let kind = match clusters {
            ..4085 => Kind::Fat12,
            ..65525 => Kind::Fat16,
            _ => Kind::Fat32,
        };
        let fat_start = start + reserved as u64;
        let root_dir_start = fat_start + (fats * fat_size) as u64;
        Some(Layout {
            start,
            root_cluster: if kind == Kind::Fat32 { u32_at(44) } else { 0 },
            kind,
            sector_size,
            sectors_per_cluster,
            fat_start,
            root_dir_start,
            root_dir_sectors,
            data_start: root_dir_start + root_dir_sectors as u64,
        })
    }

    pub fn disk_name(&self) -> &str {
        &self.disk.name
    }

    pub fn disk_size(&self) -> u64 {
        self.disk.block_count * self.disk.block_size as u64
    }

    pub fn partition_start(&self) -> u64 {
        self.l.start
    }

    /// List a directory given as a path like `/EFI/BOOT`.
    pub fn list(&mut self, path: &str) -> Result<Vec<DirEntry>, &'static str> {
        let mut entries = self.read_dir(None)?;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let dir = entries
                .into_iter()
                .find(|e| e.is_dir && e.name.eq_ignore_ascii_case(part))
                .ok_or("folder not found")?;
            entries = self.read_dir(Some(dir.cluster))?;
        }
        Ok(entries)
    }

    /// Read a whole file given as a path like `/EFI/BOOT/BOOTX64.EFI`.
    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>, &'static str> {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let entry = self
            .list(dir)?
            .into_iter()
            .find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(name))
            .ok_or("file not found")?;
        let mut data = self.read_chain(entry.cluster)?;
        data.truncate(entry.size as usize);
        Ok(data)
    }

    /// `None` means the root directory.
    fn read_dir(&mut self, cluster: Option<u32>) -> Result<Vec<DirEntry>, &'static str> {
        let raw = match cluster {
            Some(0) | None if self.l.kind != Kind::Fat32 => {
                // FAT12/16 keep the root directory in a fixed area.
                let mut buf = alloc::vec![0u8; self.l.root_dir_sectors as usize * 512];
                self.disk.read_blocks(self.l.root_dir_start, self.l.root_dir_sectors as usize, &mut buf)?;
                buf
            }
            Some(c) => self.read_chain(c)?,
            None => self.read_chain(self.l.root_cluster)?,
        };

        let mut entries = Vec::new();
        let mut long_name: Vec<u16> = Vec::new();
        for e in raw.chunks_exact(32) {
            match e[0] {
                0x00 => break, // end of directory
                0xE5 => {
                    long_name.clear(); // deleted
                    continue;
                }
                _ => {}
            }
            let attributes = e[11];
            if attributes == 0x0F {
                // Long file name piece. They come last-piece-first, 13
                // UTF-16 characters each.
                let mut piece: Vec<u16> = Vec::new();
                for range in [1..11, 14..26, 28..32] {
                    for c in e[range].chunks_exact(2) {
                        piece.push(u16::from_le_bytes([c[0], c[1]]));
                    }
                }
                piece.retain(|&c| c != 0 && c != 0xFFFF);
                piece.extend_from_slice(&long_name);
                long_name = piece;
                continue;
            }
            if attributes & 0x08 != 0 {
                long_name.clear(); // volume label
                continue;
            }
            let name = if long_name.is_empty() {
                short_name(e)
            } else {
                char::decode_utf16(long_name.iter().copied()).map(|c| c.unwrap_or('?')).collect()
            };
            long_name.clear();
            if name == "." || name == ".." {
                continue;
            }
            let cluster = (u16::from_le_bytes([e[20], e[21]]) as u32) << 16 | u16::from_le_bytes([e[26], e[27]]) as u32;
            entries.push(DirEntry {
                name,
                is_dir: attributes & 0x10 != 0,
                size: u32::from_le_bytes(e[28..32].try_into().unwrap()),
                cluster,
            });
        }
        Ok(entries)
    }

    /// Read every cluster of a file or directory, following the FAT chain.
    fn read_chain(&mut self, first: u32) -> Result<Vec<u8>, &'static str> {
        let cluster_bytes = (self.l.sectors_per_cluster * self.l.sector_size) as usize;
        let mut data = Vec::new();
        let mut cluster = first;
        let mut fat_cache: Option<(u64, [u8; 1024])> = None;
        while cluster >= 2 && !self.is_end(cluster) {
            let lba = self.l.data_start + ((cluster - 2) * self.l.sectors_per_cluster) as u64;
            let at = data.len();
            data.resize(at + cluster_bytes, 0);
            self.disk.read_blocks(lba, self.l.sectors_per_cluster as usize, &mut data[at..])?;
            cluster = self.next_cluster(cluster, &mut fat_cache)?;
            if data.len() > 64 * 1024 * 1024 {
                return Err("file too large");
            }
        }
        Ok(data)
    }

    fn is_end(&self, cluster: u32) -> bool {
        match self.l.kind {
            Kind::Fat12 => cluster >= 0xFF8,
            Kind::Fat16 => cluster >= 0xFFF8,
            Kind::Fat32 => cluster & 0x0FFF_FFFF >= 0x0FFF_FFF8,
        }
    }

    /// Look up the next cluster in the File Allocation Table. Reads two
    /// sectors at a time because FAT12 entries can straddle a boundary.
    fn next_cluster(&mut self, cluster: u32, cache: &mut Option<(u64, [u8; 1024])>) -> Result<u32, &'static str> {
        let offset = match self.l.kind {
            Kind::Fat12 => cluster + cluster / 2,
            Kind::Fat16 => cluster * 2,
            Kind::Fat32 => cluster * 4,
        } as u64;
        let lba = self.l.fat_start + offset / 512;
        if cache.as_ref().is_none_or(|(cached, _)| *cached != lba) {
            let mut buf = [0u8; 1024];
            self.disk.read_blocks(lba, 2, &mut buf)?;
            *cache = Some((lba, buf));
        }
        let buf = &cache.as_ref().unwrap().1;
        let i = (offset % 512) as usize;
        Ok(match self.l.kind {
            Kind::Fat12 => {
                let v = u16::from_le_bytes([buf[i], buf[i + 1]]) as u32;
                if cluster & 1 == 1 { v >> 4 } else { v & 0xFFF }
            }
            Kind::Fat16 => u16::from_le_bytes([buf[i], buf[i + 1]]) as u32,
            Kind::Fat32 => u32::from_le_bytes(buf[i..i + 4].try_into().unwrap()) & 0x0FFF_FFFF,
        })
    }
}

/// Old-style "NAME    EXT" names, turned into "NAME.EXT".
fn short_name(e: &[u8]) -> String {
    let base = core::str::from_utf8(&e[0..8]).unwrap_or("?").trim_end();
    let ext = core::str::from_utf8(&e[8..11]).unwrap_or("").trim_end();
    if ext.is_empty() { base.into() } else { alloc::format!("{base}.{ext}") }
}
