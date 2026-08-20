use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path};

use anyhow::{bail, Context, Result};
use csv::{Reader, ReaderBuilder, StringRecord};
use sha2::{Digest, Sha256};
use tempfile::tempfile_in;
use url::Url;
use zip::read::ZipArchive;
use zip::CompressionMethod;

use super::{
    normalize_n_number, normalize_serial_key, AircraftRecord, AircraftReference, EngineReference,
    MemberProvenance, Release, ReleaseMetadata, TargetCoverage, AIRCRAFT_MEMBER_NAME,
    ENGINE_MEMBER_NAME, MASTER_MEMBER_NAME,
};

const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 256;
const MAX_ARCHIVE_MEMBER_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_MASTER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_AIRCRAFT_REFERENCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENGINE_REFERENCE_BYTES: u64 = 16 * 1024 * 1024;
const END_OF_CENTRAL_DIRECTORY_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_END_OF_CENTRAL_DIRECTORY_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const CENTRAL_DIRECTORY_ENTRY_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_FILE_HEADER_SIGNATURE: u32 = 0x0403_4b50;
const DATA_DESCRIPTOR_SIGNATURE: u32 = 0x0807_4b50;
const ZIP64_EXTRA_FIELD_ID: u16 = 0x0001;
const END_OF_CENTRAL_DIRECTORY_BYTES: usize = 22;
const MAX_ZIP_COMMENT_BYTES: usize = u16::MAX as usize;
const CENTRAL_DIRECTORY_ENTRY_BYTES: usize = 46;
const LOCAL_FILE_HEADER_BYTES: usize = 30;
const DATA_DESCRIPTOR_BYTES_WITHOUT_SIGNATURE: usize = 12;
const DATA_DESCRIPTOR_BYTES_WITH_SIGNATURE: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
struct CentralDirectoryEntry {
    raw_name: Vec<u8>,
    flags: u16,
    compression: u16,
    modification_time: u16,
    modification_date: u16,
    crc32: u32,
    compressed_size: u32,
    uncompressed_size: u32,
    local_header_offset: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CentralDirectoryPreflight {
    entry_count: usize,
    member_names: BTreeSet<Vec<u8>>,
    snapshot_date: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RequiredArchiveMembers {
    master: usize,
    aircraft_reference: usize,
    engine_reference: usize,
}

#[cfg(test)]
pub(crate) struct ReleaseFixtureBuilder {
    metadata: ReleaseMetadata,
    source_manifest_sha256: String,
    target_set_sha256: String,
    master: MemberProvenance,
    aircraft_reference: MemberProvenance,
    engine_reference: MemberProvenance,
    coverage: Vec<TargetCoverage>,
    aircraft: Vec<AircraftRecord>,
    aircraft_references: Vec<AircraftReference>,
    engine_references: Vec<EngineReference>,
}

#[cfg(test)]
impl ReleaseFixtureBuilder {
    pub(crate) fn new(
        metadata: ReleaseMetadata,
        source_manifest_sha256: impl Into<String>,
        target_set_sha256: impl Into<String>,
        master: MemberProvenance,
        aircraft_reference: MemberProvenance,
        engine_reference: MemberProvenance,
    ) -> Self {
        Self {
            metadata,
            source_manifest_sha256: source_manifest_sha256.into(),
            target_set_sha256: target_set_sha256.into(),
            master,
            aircraft_reference,
            engine_reference,
            coverage: Vec::new(),
            aircraft: Vec::new(),
            aircraft_references: Vec::new(),
            engine_references: Vec::new(),
        }
    }

    pub(crate) fn coverage(mut self, coverage: Vec<TargetCoverage>) -> Self {
        self.coverage = coverage;
        self
    }

    pub(crate) fn aircraft(mut self, aircraft: Vec<AircraftRecord>) -> Self {
        self.aircraft = aircraft;
        self
    }

    pub(crate) fn aircraft_references(
        mut self,
        aircraft_references: Vec<AircraftReference>,
    ) -> Self {
        self.aircraft_references = aircraft_references;
        self
    }

    pub(crate) fn build(self) -> Release {
        Release {
            metadata: self.metadata,
            source_manifest_sha256: self.source_manifest_sha256,
            target_set_sha256: self.target_set_sha256,
            master: self.master,
            aircraft_reference: self.aircraft_reference,
            engine_reference: self.engine_reference,
            coverage: self.coverage,
            aircraft: self.aircraft,
            aircraft_references: self.aircraft_references,
            engine_references: self.engine_references,
        }
    }

    pub(crate) fn from_csv<M, A, E, I, S>(
        metadata: ReleaseMetadata,
        master: M,
        aircraft_reference: A,
        engine_reference: E,
        target_n_numbers: I,
    ) -> Result<Release>
    where
        M: Read,
        A: Read,
        E: Read,
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        parse_fixture_release(
            metadata,
            master,
            aircraft_reference,
            engine_reference,
            target_n_numbers,
        )
    }
}

/// Snapshot, hash, and validate one official FAA release ZIP, then stream exactly one
/// root `MASTER.txt`, `ACFTREF.txt`, and `ENGINE.txt` member into the
/// privacy-minimizing registry projection.
///
/// The complete archive hash and each uncompressed member hash are derived
/// from the same supplied bytes. No production caller can pair extracted
/// member files with unrelated provenance. The snapshot date is derived from
/// the shared, validated ZIP DOS date of the three required FAA members.
pub fn parse_release_archive<R, I, S>(archive_reader: R, target_n_numbers: I) -> Result<Release>
where
    R: Read,
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    parse_release_archive_in(
        archive_reader,
        target_n_numbers,
        &std::env::temp_dir(),
        MAX_ARCHIVE_BYTES,
    )
}

fn parse_release_archive_in<R, I, S>(
    archive_reader: R,
    target_n_numbers: I,
    temporary_directory: &Path,
    maximum_archive_bytes: u64,
) -> Result<Release>
where
    R: Read,
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let (mut archive_snapshot, archive_sha256) =
        snapshot_archive(archive_reader, temporary_directory, maximum_archive_bytes)?;
    let targets = normalize_targets(target_n_numbers)?;
    let target_set_sha256 = target_set_digest(&targets);

    let preflight = preflight_central_directory(&mut archive_snapshot)?;
    let mut metadata = ReleaseMetadata::official(preflight.snapshot_date.clone(), archive_sha256);
    validate_snapshot_metadata(&mut metadata)?;
    archive_snapshot
        .seek(SeekFrom::Start(0))
        .context("FAA release ZIP could not be rewound after structural validation")?;
    let mut archive =
        ZipArchive::new(archive_snapshot).context("FAA release is not a valid ZIP")?;
    let members = validate_archive(&mut archive, preflight)?;

    let (aircraft, coverage, master_sha256) = {
        let master = archive_member(
            &mut archive,
            members.master,
            MASTER_MEMBER_NAME,
            MAX_MASTER_BYTES,
        )?;
        parse_master(master, &targets)?
    };
    let aircraft_codes = aircraft
        .iter()
        .map(|record| record.aircraft_code.as_str())
        .collect::<BTreeSet<_>>();
    let engine_codes = aircraft
        .iter()
        .filter_map(|record| record.engine_code.as_deref())
        .collect::<BTreeSet<_>>();
    let (aircraft_references, aircraft_sha256) = {
        let aircraft_reference = archive_member(
            &mut archive,
            members.aircraft_reference,
            AIRCRAFT_MEMBER_NAME,
            MAX_AIRCRAFT_REFERENCE_BYTES,
        )?;
        parse_aircraft_references(aircraft_reference, &aircraft_codes)?
    };
    let (engine_references, engine_sha256) = {
        let engine_reference = archive_member(
            &mut archive,
            members.engine_reference,
            ENGINE_MEMBER_NAME,
            MAX_ENGINE_REFERENCE_BYTES,
        )?;
        parse_engine_references(engine_reference, &engine_codes)?
    };

    Ok(assemble_release(
        metadata,
        target_set_sha256,
        aircraft,
        coverage,
        aircraft_references,
        engine_references,
        master_sha256,
        aircraft_sha256,
        engine_sha256,
    ))
}

#[cfg(test)]
fn parse_fixture_release<M, A, E, I, S>(
    mut metadata: ReleaseMetadata,
    master: M,
    aircraft_reference: A,
    engine_reference: E,
    target_n_numbers: I,
) -> Result<Release>
where
    M: Read,
    A: Read,
    E: Read,
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    validate_snapshot_metadata(&mut metadata)?;

    let targets = normalize_targets(target_n_numbers)?;
    let target_set_sha256 = target_set_digest(&targets);
    let (aircraft, coverage, master_sha256) = parse_master(master, &targets)?;
    let aircraft_codes = aircraft
        .iter()
        .map(|record| record.aircraft_code.as_str())
        .collect::<BTreeSet<_>>();
    let engine_codes = aircraft
        .iter()
        .filter_map(|record| record.engine_code.as_deref())
        .collect::<BTreeSet<_>>();
    let (aircraft_references, aircraft_sha256) =
        parse_aircraft_references(aircraft_reference, &aircraft_codes)?;
    let (engine_references, engine_sha256) =
        parse_engine_references(engine_reference, &engine_codes)?;

    Ok(assemble_release(
        metadata,
        target_set_sha256,
        aircraft,
        coverage,
        aircraft_references,
        engine_references,
        master_sha256,
        aircraft_sha256,
        engine_sha256,
    ))
}

fn assemble_release(
    metadata: ReleaseMetadata,
    target_set_sha256: String,
    aircraft: Vec<AircraftRecord>,
    coverage: Vec<TargetCoverage>,
    aircraft_references: Vec<AircraftReference>,
    engine_references: Vec<EngineReference>,
    master_sha256: String,
    aircraft_sha256: String,
    engine_sha256: String,
) -> Release {
    let master = MemberProvenance {
        member_name: MASTER_MEMBER_NAME.to_string(),
        sha256: master_sha256,
    };
    let aircraft_reference = MemberProvenance {
        member_name: AIRCRAFT_MEMBER_NAME.to_string(),
        sha256: aircraft_sha256,
    };
    let engine_reference = MemberProvenance {
        member_name: ENGINE_MEMBER_NAME.to_string(),
        sha256: engine_sha256,
    };
    let source_manifest_sha256 =
        source_manifest_digest(&metadata, [&master, &aircraft_reference, &engine_reference]);

    Release {
        metadata,
        source_manifest_sha256,
        target_set_sha256,
        master,
        aircraft_reference,
        engine_reference,
        coverage,
        aircraft,
        aircraft_references,
        engine_references,
    }
}

fn snapshot_archive<R: Read>(
    mut reader: R,
    temporary_directory: &Path,
    maximum_archive_bytes: u64,
) -> Result<(File, String)> {
    let mut snapshot = tempfile_in(temporary_directory)
        .context("private FAA release ZIP snapshot could not be created")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        snapshot
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .context("private FAA release ZIP snapshot permissions could not be restricted")?;
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut archive_size = 0_u64;
    loop {
        let remaining_with_probe = maximum_archive_bytes
            .saturating_sub(archive_size)
            .saturating_add(1);
        let read_limit = usize::try_from(remaining_with_probe.min(buffer.len() as u64)).unwrap();
        let count = reader
            .read(&mut buffer[..read_limit])
            .context("FAA release ZIP could not be snapshotted")?;
        if count == 0 {
            break;
        }
        archive_size = archive_size
            .checked_add(u64::try_from(count).unwrap())
            .context("FAA release ZIP size overflowed")?;
        if archive_size > maximum_archive_bytes {
            bail!(
                "FAA release ZIP exceeds the maximum accepted size of {maximum_archive_bytes} bytes"
            );
        }
        digest.update(&buffer[..count]);
        snapshot
            .write_all(&buffer[..count])
            .context("private FAA release ZIP snapshot could not be written")?;
    }
    if archive_size == 0 {
        bail!("FAA release ZIP is empty");
    }
    snapshot
        .flush()
        .context("private FAA release ZIP snapshot could not be flushed")?;
    snapshot
        .seek(SeekFrom::Start(0))
        .context("private FAA release ZIP snapshot could not be rewound")?;
    Ok((snapshot, format!("{:x}", digest.finalize())))
}

fn preflight_central_directory<R: Read + Seek>(
    reader: &mut R,
) -> Result<CentralDirectoryPreflight> {
    let archive_size = reader
        .seek(SeekFrom::End(0))
        .context("FAA release ZIP size could not be inspected")?;
    let tail_size = archive_size
        .min(u64::try_from(END_OF_CENTRAL_DIRECTORY_BYTES + MAX_ZIP_COMMENT_BYTES).unwrap());
    if tail_size < u64::try_from(END_OF_CENTRAL_DIRECTORY_BYTES).unwrap() {
        bail!("FAA release ZIP is too short to contain a central directory");
    }
    reader
        .seek(SeekFrom::End(-i64::try_from(tail_size).unwrap()))
        .context("FAA release ZIP end record could not be located")?;
    let mut tail = vec![0_u8; usize::try_from(tail_size).unwrap()];
    reader
        .read_exact(&mut tail)
        .context("FAA release ZIP end record could not be read")?;

    let eocd_tail_offset = (0..=tail.len().saturating_sub(END_OF_CENTRAL_DIRECTORY_BYTES))
        .rev()
        .find(|offset| {
            little_u32(&tail[*offset..*offset + 4]) == END_OF_CENTRAL_DIRECTORY_SIGNATURE
                && *offset
                    + END_OF_CENTRAL_DIRECTORY_BYTES
                    + usize::from(little_u16(&tail[*offset + 20..*offset + 22]))
                    == tail.len()
        })
        .context("FAA release ZIP has no valid end-of-central-directory record")?;
    if eocd_tail_offset >= 20
        && little_u32(&tail[eocd_tail_offset - 20..eocd_tail_offset - 16])
            == ZIP64_END_OF_CENTRAL_DIRECTORY_LOCATOR_SIGNATURE
    {
        bail!("FAA release ZIP64 archives are not supported");
    }
    let eocd = &tail[eocd_tail_offset..eocd_tail_offset + END_OF_CENTRAL_DIRECTORY_BYTES];
    let disk_number = little_u16(&eocd[4..6]);
    let central_directory_disk = little_u16(&eocd[6..8]);
    let entries_on_disk = little_u16(&eocd[8..10]);
    let total_entries = little_u16(&eocd[10..12]);
    let central_directory_size = little_u32(&eocd[12..16]);
    let central_directory_offset = little_u32(&eocd[16..20]);
    if disk_number == u16::MAX
        || central_directory_disk == u16::MAX
        || entries_on_disk == u16::MAX
        || total_entries == u16::MAX
        || central_directory_size == u32::MAX
        || central_directory_offset == u32::MAX
    {
        bail!("FAA release ZIP64 archives are not supported");
    }
    if disk_number != 0 || central_directory_disk != 0 || entries_on_disk != total_entries {
        bail!("FAA release ZIP must be a single-disk archive");
    }
    let entry_count = usize::from(total_entries);
    if entry_count == 0 {
        bail!("FAA release ZIP contains no entries");
    }
    if entry_count > MAX_ARCHIVE_ENTRIES {
        bail!(
            "FAA release ZIP contains {entry_count} entries; maximum accepted count is {MAX_ARCHIVE_ENTRIES}"
        );
    }

    let tail_start = archive_size - tail_size;
    let eocd_offset = tail_start + u64::try_from(eocd_tail_offset).unwrap();
    let directory_offset = u64::from(central_directory_offset);
    let directory_size = u64::from(central_directory_size);
    if directory_offset
        .checked_add(directory_size)
        .filter(|end| *end == eocd_offset)
        .is_none()
    {
        bail!("FAA release ZIP central-directory bounds are inconsistent");
    }
    reader
        .seek(SeekFrom::Start(directory_offset))
        .context("FAA release ZIP central directory could not be opened")?;
    let mut names = BTreeSet::new();
    let mut entries = Vec::with_capacity(entry_count);
    for index in 0..entry_count {
        let mut header = [0_u8; CENTRAL_DIRECTORY_ENTRY_BYTES];
        reader
            .read_exact(&mut header)
            .with_context(|| format!("FAA release ZIP central entry {index} is truncated"))?;
        if little_u32(&header[0..4]) != CENTRAL_DIRECTORY_ENTRY_SIGNATURE {
            bail!("FAA release ZIP central entry {index} has an invalid signature");
        }
        let flags = little_u16(&header[8..10]);
        let compression = little_u16(&header[10..12]);
        let modification_time = little_u16(&header[12..14]);
        let modification_date = little_u16(&header[14..16]);
        validate_dos_timestamp(modification_date, modification_time).with_context(|| {
            format!("FAA release ZIP central entry {index} timestamp is invalid")
        })?;
        let crc32 = little_u32(&header[16..20]);
        let compressed_size = little_u32(&header[20..24]);
        let uncompressed_size = little_u32(&header[24..28]);
        let disk_start = little_u16(&header[34..36]);
        let local_header_offset = little_u32(&header[42..46]);
        if disk_start == u16::MAX
            || compressed_size == u32::MAX
            || uncompressed_size == u32::MAX
            || local_header_offset == u32::MAX
        {
            bail!("FAA release ZIP64 per-entry fields are not supported");
        }
        if disk_start != 0 {
            bail!("FAA release ZIP must be a single-disk archive");
        }
        let name_length = usize::from(little_u16(&header[28..30]));
        let extra_length = usize::from(little_u16(&header[30..32]));
        let comment_length = usize::from(little_u16(&header[32..34]));
        if name_length == 0 {
            bail!("FAA release ZIP central entry {index} has an empty name");
        }
        let mut raw_name = vec![0_u8; name_length];
        reader
            .read_exact(&mut raw_name)
            .with_context(|| format!("FAA release ZIP central entry {index} name is truncated"))?;
        let name = std::str::from_utf8(&raw_name)
            .with_context(|| format!("FAA release ZIP central entry {index} name is not UTF-8"))?;
        validate_archive_member_path(name)?;
        if names.contains(&raw_name) {
            bail!("FAA release ZIP contains duplicate member {name:?}");
        }
        names.insert(raw_name.clone());
        let mut extra = vec![0_u8; extra_length];
        reader.read_exact(&mut extra).with_context(|| {
            format!("FAA release ZIP central entry {index} extra field is truncated")
        })?;
        validate_extra_fields(&extra, &format!("central entry {index}"))?;
        reader
            .seek(SeekFrom::Current(i64::try_from(comment_length).unwrap()))
            .with_context(|| {
                format!("FAA release ZIP central entry {index} comment is truncated")
            })?;
        if reader
            .stream_position()
            .context("FAA release ZIP central-directory position could not be inspected")?
            > eocd_offset
        {
            bail!("FAA release ZIP central entry {index} exceeds central-directory bounds");
        }
        entries.push(CentralDirectoryEntry {
            raw_name,
            flags,
            compression,
            modification_time,
            modification_date,
            crc32,
            compressed_size,
            uncompressed_size,
            local_header_offset,
        });
    }
    let directory_end = reader
        .stream_position()
        .context("FAA release ZIP central-directory position could not be inspected")?;
    if directory_end != eocd_offset {
        bail!("FAA release ZIP central-directory size does not match its entries");
    }
    validate_local_entries(reader, directory_offset, &entries)?;
    let snapshot_date = required_member_snapshot_date(&entries)?;

    Ok(CentralDirectoryPreflight {
        entry_count,
        member_names: names,
        snapshot_date,
    })
}

fn required_member_snapshot_date(entries: &[CentralDirectoryEntry]) -> Result<String> {
    let mut required_dates = Vec::with_capacity(3);
    for required_name in [
        MASTER_MEMBER_NAME.as_bytes(),
        AIRCRAFT_MEMBER_NAME.as_bytes(),
        ENGINE_MEMBER_NAME.as_bytes(),
    ] {
        let entry = entries
            .iter()
            .find(|entry| entry.raw_name == required_name)
            .with_context(|| {
                format!(
                    "FAA release ZIP is missing root {}",
                    String::from_utf8_lossy(required_name)
                )
            })?;
        required_dates.push((required_name, entry.modification_date));
    }
    let expected = required_dates[0].1;
    if required_dates.iter().any(|(_, date)| *date != expected) {
        let rendered = required_dates
            .iter()
            .map(|(name, date)| {
                let date =
                    dos_date_string(*date).unwrap_or_else(|_| format!("invalid({date:#06x})"));
                format!("{}={date}", String::from_utf8_lossy(name))
            })
            .collect::<Vec<_>>()
            .join(", ");
        bail!("FAA required archive members do not share one release date: {rendered}");
    }
    dos_date_string(expected)
}

fn validate_dos_timestamp(date: u16, time: u16) -> Result<()> {
    dos_date_string(date)?;
    let seconds = (time & 0x1f) * 2;
    let minutes = (time >> 5) & 0x3f;
    let hours = (time >> 11) & 0x1f;
    if seconds > 59 || minutes > 59 || hours > 23 {
        bail!("ZIP DOS time is outside the calendar range");
    }
    Ok(())
}

fn dos_date_string(date: u16) -> Result<String> {
    let day = date & 0x1f;
    let month = (date >> 5) & 0x0f;
    let year = 1980_u16 + (date >> 9);
    let maximum_day = match month {
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => bail!("ZIP DOS date has an invalid month"),
    };
    if day == 0 || day > maximum_day {
        bail!("ZIP DOS date has an invalid day for its month");
    }
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

fn validate_extra_fields(extra: &[u8], location: &str) -> Result<()> {
    let mut offset = 0_usize;
    while offset < extra.len() {
        if extra.len() - offset < 4 {
            bail!("FAA release ZIP {location} has a truncated extra-field header");
        }
        let field_id = little_u16(&extra[offset..offset + 2]);
        let field_size = usize::from(little_u16(&extra[offset + 2..offset + 4]));
        offset = offset
            .checked_add(4)
            .and_then(|value| value.checked_add(field_size))
            .context("FAA release ZIP extra-field length overflowed")?;
        if offset > extra.len() {
            bail!("FAA release ZIP {location} has a truncated extra field");
        }
        if field_id == ZIP64_EXTRA_FIELD_ID {
            bail!("FAA release ZIP64 per-entry extra fields are not supported");
        }
    }
    Ok(())
}

fn validate_local_entries<R: Read + Seek>(
    reader: &mut R,
    central_directory_offset: u64,
    entries: &[CentralDirectoryEntry],
) -> Result<()> {
    let mut ranges = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let local_offset = u64::from(entry.local_header_offset);
        let fixed_end = local_offset
            .checked_add(u64::try_from(LOCAL_FILE_HEADER_BYTES).unwrap())
            .context("FAA release ZIP local-header bounds overflowed")?;
        if fixed_end > central_directory_offset {
            bail!("FAA release ZIP local entry {index} starts outside file-data bounds");
        }
        reader
            .seek(SeekFrom::Start(local_offset))
            .with_context(|| format!("FAA release ZIP local entry {index} could not be opened"))?;
        let mut header = [0_u8; LOCAL_FILE_HEADER_BYTES];
        reader
            .read_exact(&mut header)
            .with_context(|| format!("FAA release ZIP local entry {index} is truncated"))?;
        if little_u32(&header[0..4]) != LOCAL_FILE_HEADER_SIGNATURE {
            bail!("FAA release ZIP local entry {index} has an invalid signature");
        }
        let local_flags = little_u16(&header[6..8]);
        let local_compression = little_u16(&header[8..10]);
        let local_modification_time = little_u16(&header[10..12]);
        let local_modification_date = little_u16(&header[12..14]);
        let local_crc32 = little_u32(&header[14..18]);
        let local_compressed_size = little_u32(&header[18..22]);
        let local_uncompressed_size = little_u32(&header[22..26]);
        if local_compressed_size == u32::MAX || local_uncompressed_size == u32::MAX {
            bail!("FAA release ZIP64 local-entry fields are not supported");
        }
        if local_flags != entry.flags
            || local_compression != entry.compression
            || local_modification_time != entry.modification_time
            || local_modification_date != entry.modification_date
        {
            bail!("FAA release ZIP local entry {index} disagrees with its central entry");
        }

        let name_length = usize::from(little_u16(&header[26..28]));
        let extra_length = usize::from(little_u16(&header[28..30]));
        let variable_end = fixed_end
            .checked_add(u64::try_from(name_length).unwrap())
            .and_then(|value| value.checked_add(u64::try_from(extra_length).unwrap()))
            .context("FAA release ZIP local-entry metadata bounds overflowed")?;
        if variable_end > central_directory_offset {
            bail!("FAA release ZIP local entry {index} metadata exceeds file-data bounds");
        }
        let mut raw_name = vec![0_u8; name_length];
        reader
            .read_exact(&mut raw_name)
            .with_context(|| format!("FAA release ZIP local entry {index} name is truncated"))?;
        if raw_name != entry.raw_name {
            bail!("FAA release ZIP local entry {index} name disagrees with its central entry");
        }
        let mut extra = vec![0_u8; extra_length];
        reader.read_exact(&mut extra).with_context(|| {
            format!("FAA release ZIP local entry {index} extra field is truncated")
        })?;
        validate_extra_fields(&extra, &format!("local entry {index}"))?;

        let uses_descriptor = entry.flags & (1 << 3) != 0;
        let exact_local_sizes = local_crc32 == entry.crc32
            && local_compressed_size == entry.compressed_size
            && local_uncompressed_size == entry.uncompressed_size;
        if uses_descriptor {
            let empty_local_sizes =
                local_crc32 == 0 && local_compressed_size == 0 && local_uncompressed_size == 0;
            if !empty_local_sizes && !exact_local_sizes {
                bail!("FAA release ZIP descriptor entry {index} has inconsistent local sizes");
            }
        } else if !exact_local_sizes {
            bail!("FAA release ZIP local entry {index} sizes disagree with its central entry");
        }

        let data_end = variable_end
            .checked_add(u64::from(entry.compressed_size))
            .context("FAA release ZIP compressed-data bounds overflowed")?;
        if data_end > central_directory_offset {
            bail!("FAA release ZIP local entry {index} data exceeds file-data bounds");
        }
        let entry_end = if uses_descriptor {
            validate_data_descriptor(reader, index, data_end, central_directory_offset, entry)?
        } else {
            data_end
        };
        ranges.push((local_offset, entry_end, index));
    }

    ranges.sort_unstable_by_key(|range| range.0);
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            bail!(
                "FAA release ZIP local entries {} and {} overlap",
                pair[0].2,
                pair[1].2
            );
        }
    }
    Ok(())
}

fn validate_data_descriptor<R: Read + Seek>(
    reader: &mut R,
    index: usize,
    descriptor_offset: u64,
    central_directory_offset: u64,
    entry: &CentralDirectoryEntry,
) -> Result<u64> {
    let minimum_end = descriptor_offset
        .checked_add(u64::try_from(DATA_DESCRIPTOR_BYTES_WITHOUT_SIGNATURE).unwrap())
        .context("FAA release ZIP data-descriptor bounds overflowed")?;
    if minimum_end > central_directory_offset {
        bail!("FAA release ZIP data descriptor {index} exceeds file-data bounds");
    }
    reader
        .seek(SeekFrom::Start(descriptor_offset))
        .with_context(|| format!("FAA release ZIP data descriptor {index} could not be opened"))?;
    let mut descriptor = [0_u8; DATA_DESCRIPTOR_BYTES_WITH_SIGNATURE];
    reader
        .read_exact(&mut descriptor[..DATA_DESCRIPTOR_BYTES_WITHOUT_SIGNATURE])
        .with_context(|| format!("FAA release ZIP data descriptor {index} is truncated"))?;
    let unsigned_matches = little_u32(&descriptor[0..4]) == entry.crc32
        && little_u32(&descriptor[4..8]) == entry.compressed_size
        && little_u32(&descriptor[8..12]) == entry.uncompressed_size;
    if unsigned_matches {
        return Ok(minimum_end);
    }
    if little_u32(&descriptor[0..4]) != DATA_DESCRIPTOR_SIGNATURE {
        bail!("FAA release ZIP data descriptor {index} disagrees with its central entry");
    }
    let signed_end = descriptor_offset
        .checked_add(u64::try_from(DATA_DESCRIPTOR_BYTES_WITH_SIGNATURE).unwrap())
        .context("FAA release ZIP data-descriptor bounds overflowed")?;
    if signed_end > central_directory_offset {
        bail!("FAA release ZIP data descriptor {index} exceeds file-data bounds");
    }
    reader
        .read_exact(&mut descriptor[DATA_DESCRIPTOR_BYTES_WITHOUT_SIGNATURE..])
        .with_context(|| format!("FAA release ZIP data descriptor {index} is truncated"))?;
    if little_u32(&descriptor[4..8]) != entry.crc32
        || little_u32(&descriptor[8..12]) != entry.compressed_size
        || little_u32(&descriptor[12..16]) != entry.uncompressed_size
    {
        bail!("FAA release ZIP data descriptor {index} disagrees with its central entry");
    }
    Ok(signed_end)
}

fn little_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().expect("two-byte ZIP field"))
}

fn little_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four-byte ZIP field"))
}

fn validate_archive<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    preflight: CentralDirectoryPreflight,
) -> Result<RequiredArchiveMembers> {
    if archive.len() != preflight.entry_count {
        bail!(
            "FAA release ZIP reader exposed {} entries after structural validation found {}",
            archive.len(),
            preflight.entry_count
        );
    }
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        bail!(
            "FAA release ZIP contains {} entries; maximum accepted count is {MAX_ARCHIVE_ENTRIES}",
            archive.len()
        );
    }

    let mut master = None;
    let mut aircraft_reference = None;
    let mut engine_reference = None;
    let mut exposed_names = BTreeSet::new();
    for index in 0..archive.len() {
        let member = archive
            .by_index_raw(index)
            .with_context(|| format!("FAA release ZIP entry {index} could not be inspected"))?;
        exposed_names.insert(member.name_raw().to_vec());
        validate_archive_member_path(member.name())?;
        if member.encrypted() {
            bail!("FAA release ZIP member {:?} is encrypted", member.name());
        }
        if member.size() > MAX_ARCHIVE_MEMBER_BYTES {
            bail!(
                "FAA release ZIP member {:?} is {} bytes; maximum accepted member size is {MAX_ARCHIVE_MEMBER_BYTES}",
                member.name(),
                member.size()
            );
        }
        let required = match member.name() {
            MASTER_MEMBER_NAME => Some((&mut master, MAX_MASTER_BYTES)),
            AIRCRAFT_MEMBER_NAME => Some((&mut aircraft_reference, MAX_AIRCRAFT_REFERENCE_BYTES)),
            ENGINE_MEMBER_NAME => Some((&mut engine_reference, MAX_ENGINE_REFERENCE_BYTES)),
            _ => None,
        };
        let Some((slot, maximum_size)) = required else {
            continue;
        };
        if !member.is_file() || member.is_symlink() {
            bail!(
                "FAA release ZIP required member {:?} is not a regular file",
                member.name()
            );
        }
        if member.size() == 0 || member.size() > maximum_size {
            bail!(
                "FAA release ZIP required member {:?} is {} bytes; accepted range is 1..={maximum_size}",
                member.name(),
                member.size()
            );
        }
        if !matches!(
            member.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            bail!(
                "FAA release ZIP required member {:?} uses unsupported compression {:?}",
                member.name(),
                member.compression()
            );
        }
        if slot.replace(index).is_some() {
            bail!(
                "FAA release ZIP contains duplicate root member {:?}",
                member.name()
            );
        }
    }
    if exposed_names != preflight.member_names {
        bail!("FAA release ZIP member names changed between structural validation and reading");
    }

    Ok(RequiredArchiveMembers {
        master: master.context("FAA release ZIP is missing root MASTER.txt")?,
        aircraft_reference: aircraft_reference
            .context("FAA release ZIP is missing root ACFTREF.txt")?,
        engine_reference: engine_reference.context("FAA release ZIP is missing root ENGINE.txt")?,
    })
}

fn validate_archive_member_path(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('\0')
        || name.contains('\\')
        || name.split('/').any(|component| component == "..")
        || std::path::Path::new(name)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("FAA release ZIP contains unsafe member path {name:?}");
    }
    Ok(())
}

fn archive_member<'a, R: Read + Seek>(
    archive: &'a mut ZipArchive<R>,
    index: usize,
    expected_name: &str,
    maximum_size: u64,
) -> Result<SizeLimitedReader<zip::read::ZipFile<'a, R>>> {
    let member = archive
        .by_index(index)
        .with_context(|| format!("FAA release ZIP member {expected_name} could not be opened"))?;
    if member.name() != expected_name {
        bail!(
            "FAA release ZIP member index {index} changed from {expected_name:?} to {:?}",
            member.name()
        );
    }
    Ok(SizeLimitedReader::new(member, maximum_size, expected_name))
}

struct SizeLimitedReader<R> {
    inner: R,
    remaining: u64,
    member_name: String,
}

impl<R> SizeLimitedReader<R> {
    fn new(inner: R, maximum_size: u64, member_name: impl Into<String>) -> Self {
        Self {
            inner,
            remaining: maximum_size,
            member_name: member_name.into(),
        }
    }
}

impl<R: Read> Read for SizeLimitedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut probe = [0_u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "FAA release ZIP member {:?} exceeds its accepted uncompressed size",
                        self.member_name
                    ),
                )),
            };
        }
        let accepted = usize::try_from(self.remaining.min(buffer.len() as u64)).unwrap();
        let count = self.inner.read(&mut buffer[..accepted])?;
        self.remaining -= u64::try_from(count).unwrap();
        Ok(count)
    }
}

fn validate_snapshot_metadata(metadata: &mut ReleaseMetadata) -> Result<()> {
    validate_snapshot_date(&metadata.snapshot_date)?;
    validate_faa_source_url(&metadata.source_url)?;
    metadata.archive_sha256 = normalize_digest(&metadata.archive_sha256, "archive")?;
    Ok(())
}

fn parse_master<R: Read>(
    reader: R,
    targets: &BTreeSet<String>,
) -> Result<(Vec<AircraftRecord>, Vec<TargetCoverage>, String)> {
    let mut source = DigestReader::new(reader);
    let mut rows = Vec::new();
    let mut matched_registrations = BTreeSet::new();
    {
        let mut csv = csv_reader(&mut source);
        let headers = csv
            .headers()
            .context("FAA MASTER header could not be read")?
            .clone();
        let n_number = required_column(&headers, "N-NUMBER", MASTER_MEMBER_NAME)?;
        let serial = required_column(&headers, "SERIAL NUMBER", MASTER_MEMBER_NAME)?;
        let aircraft_code = required_column(&headers, "MFR MDL CODE", MASTER_MEMBER_NAME)?;
        let engine_code = required_column(&headers, "ENG MFR MDL", MASTER_MEMBER_NAME)?;
        let year = required_column(&headers, "YEAR MFR", MASTER_MEMBER_NAME)?;

        for (offset, result) in csv.records().enumerate() {
            let record = result.with_context(|| {
                format!(
                    "FAA MASTER row {} is not valid CSV",
                    offset.saturating_add(2)
                )
            })?;
            let raw_registration = field(&record, n_number);
            let registration_input = if raw_registration
                .chars()
                .next()
                .is_some_and(|character| character.eq_ignore_ascii_case(&'N'))
            {
                raw_registration.to_string()
            } else {
                format!("N{raw_registration}")
            };
            let normalized_registration =
                normalize_n_number(&registration_input).with_context(|| {
                    format!(
                        "FAA MASTER row {} has invalid N-number {:?}",
                        offset.saturating_add(2),
                        raw_registration
                    )
                })?;
            if !targets.contains(&normalized_registration) {
                continue;
            }
            if !matched_registrations.insert(normalized_registration.clone()) {
                bail!(
                    "FAA MASTER contains duplicate normalized N-number {normalized_registration}"
                );
            }

            let manufacturer_serial_raw = optional_text(&record, serial);
            let manufacturer_serial_key = manufacturer_serial_raw
                .as_deref()
                .and_then(normalize_serial_key);
            let aircraft_code = required_text(
                &record,
                aircraft_code,
                MASTER_MEMBER_NAME,
                offset.saturating_add(2),
            )?;
            let year_manufactured = parse_year(field(&record, year))
                .with_context(|| format!("FAA MASTER row {} YEAR MFR", offset.saturating_add(2)))?;

            rows.push(AircraftRecord {
                n_number: normalized_registration,
                manufacturer_serial_raw,
                manufacturer_serial_key,
                aircraft_code,
                engine_code: optional_text(&record, engine_code),
                year_manufactured,
                source_record_sha256: logical_record_digest(&record),
            });
        }
    }
    let coverage = targets
        .iter()
        .map(|n_number| TargetCoverage {
            n_number: n_number.clone(),
            matched: matched_registrations.contains(n_number),
        })
        .collect();
    Ok((rows, coverage, source.finalize()))
}

fn parse_aircraft_references<R: Read>(
    reader: R,
    reachable_codes: &BTreeSet<&str>,
) -> Result<(Vec<AircraftReference>, String)> {
    let mut source = DigestReader::new(reader);
    let mut rows = Vec::new();
    let mut codes = BTreeSet::new();
    {
        let mut csv = csv_reader(&mut source);
        let headers = csv
            .headers()
            .context("FAA ACFTREF header could not be read")?
            .clone();
        let code = required_column(&headers, "CODE", AIRCRAFT_MEMBER_NAME)?;
        let manufacturer = required_column(&headers, "MFR", AIRCRAFT_MEMBER_NAME)?;
        let model = required_column(&headers, "MODEL", AIRCRAFT_MEMBER_NAME)?;
        let aircraft_type = required_column(&headers, "TYPE-ACFT", AIRCRAFT_MEMBER_NAME)?;
        let engine_type = required_column(&headers, "TYPE-ENG", AIRCRAFT_MEMBER_NAME)?;
        let category = required_column(&headers, "AC-CAT", AIRCRAFT_MEMBER_NAME)?;
        let certification = required_column(&headers, "BUILD-CERT-IND", AIRCRAFT_MEMBER_NAME)?;
        let engine_count = required_column(&headers, "NO-ENG", AIRCRAFT_MEMBER_NAME)?;
        let seat_count = required_column(&headers, "NO-SEATS", AIRCRAFT_MEMBER_NAME)?;
        let weight = required_column(&headers, "AC-WEIGHT", AIRCRAFT_MEMBER_NAME)?;
        let speed = required_column(&headers, "SPEED", AIRCRAFT_MEMBER_NAME)?;
        let type_certificate = required_column(&headers, "TC-DATA-SHEET", AIRCRAFT_MEMBER_NAME)?;
        let certificate_holder = required_column(&headers, "TC-DATA-HOLDER", AIRCRAFT_MEMBER_NAME)?;

        for (offset, result) in csv.records().enumerate() {
            let record = result.with_context(|| {
                format!(
                    "FAA ACFTREF row {} is not valid CSV",
                    offset.saturating_add(2)
                )
            })?;
            let aircraft_code = required_text(
                &record,
                code,
                AIRCRAFT_MEMBER_NAME,
                offset.saturating_add(2),
            )?;
            if !reachable_codes.contains(aircraft_code.as_str()) {
                continue;
            }
            if !codes.insert(aircraft_code.clone()) {
                bail!("FAA ACFTREF contains duplicate aircraft code {aircraft_code}");
            }
            rows.push(AircraftReference {
                aircraft_code,
                manufacturer_name: optional_text(&record, manufacturer),
                model_name: optional_text(&record, model),
                aircraft_type_code: optional_text(&record, aircraft_type),
                engine_type_code: optional_text(&record, engine_type),
                category_code: optional_text(&record, category),
                certification_indicator_code: optional_text(&record, certification),
                engine_count: parse_number(field(&record, engine_count)).with_context(|| {
                    format!("FAA ACFTREF row {} NO-ENG", offset.saturating_add(2))
                })?,
                seat_count: parse_number(field(&record, seat_count)).with_context(|| {
                    format!("FAA ACFTREF row {} NO-SEATS", offset.saturating_add(2))
                })?,
                weight_class_code: optional_text(&record, weight),
                cruise_speed_mph: parse_number(field(&record, speed)).with_context(|| {
                    format!("FAA ACFTREF row {} SPEED", offset.saturating_add(2))
                })?,
                type_certificate_data_sheet: optional_text(&record, type_certificate),
                type_certificate_holder: optional_text(&record, certificate_holder),
            });
        }
    }
    Ok((rows, source.finalize()))
}

fn parse_engine_references<R: Read>(
    reader: R,
    reachable_codes: &BTreeSet<&str>,
) -> Result<(Vec<EngineReference>, String)> {
    let mut source = DigestReader::new(reader);
    let mut rows = Vec::new();
    let mut codes = BTreeSet::new();
    {
        let mut csv = csv_reader(&mut source);
        let headers = csv
            .headers()
            .context("FAA ENGINE header could not be read")?
            .clone();
        let code = required_column(&headers, "CODE", ENGINE_MEMBER_NAME)?;
        let manufacturer = required_column(&headers, "MFR", ENGINE_MEMBER_NAME)?;
        let model = required_column(&headers, "MODEL", ENGINE_MEMBER_NAME)?;
        let engine_type = required_column(&headers, "TYPE", ENGINE_MEMBER_NAME)?;
        let horsepower = required_column(&headers, "HORSEPOWER", ENGINE_MEMBER_NAME)?;
        let thrust = required_column(&headers, "THRUST", ENGINE_MEMBER_NAME)?;

        for (offset, result) in csv.records().enumerate() {
            let record = result.with_context(|| {
                format!(
                    "FAA ENGINE row {} is not valid CSV",
                    offset.saturating_add(2)
                )
            })?;
            let engine_code =
                required_text(&record, code, ENGINE_MEMBER_NAME, offset.saturating_add(2))?;
            if !reachable_codes.contains(engine_code.as_str()) {
                continue;
            }
            if !codes.insert(engine_code.clone()) {
                bail!("FAA ENGINE contains duplicate engine code {engine_code}");
            }
            rows.push(EngineReference {
                engine_code,
                manufacturer_name: optional_text(&record, manufacturer),
                model_name: optional_text(&record, model),
                engine_type_code: optional_text(&record, engine_type),
                horsepower: parse_number(field(&record, horsepower)).with_context(|| {
                    format!("FAA ENGINE row {} HORSEPOWER", offset.saturating_add(2))
                })?,
                thrust_pounds: parse_number(field(&record, thrust)).with_context(|| {
                    format!("FAA ENGINE row {} THRUST", offset.saturating_add(2))
                })?,
            });
        }
    }
    Ok((rows, source.finalize()))
}

fn csv_reader<R: Read>(reader: R) -> Reader<R> {
    ReaderBuilder::new().flexible(true).from_reader(reader)
}

fn normalized_header(header: &str) -> String {
    header
        .trim_start_matches('\u{feff}')
        .trim()
        .to_ascii_uppercase()
}

fn required_column(headers: &StringRecord, name: &str, member: &str) -> Result<usize> {
    headers
        .iter()
        .position(|header| normalized_header(header) == name)
        .with_context(|| format!("FAA {member} is missing required column {name}"))
}

fn field(record: &StringRecord, index: usize) -> &str {
    record.get(index).unwrap_or_default().trim()
}

fn optional_text(record: &StringRecord, index: usize) -> Option<String> {
    let value = field(record, index);
    (!value.is_empty()).then(|| value.to_string())
}

fn required_text(
    record: &StringRecord,
    index: usize,
    member: &str,
    row_number: usize,
) -> Result<String> {
    optional_text(record, index)
        .with_context(|| format!("FAA {member} row {row_number} has an empty required value"))
}

fn parse_number<T>(value: &str) -> Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse()
            .map(Some)
            .with_context(|| format!("{value:?} is not a valid number"))
    }
}

fn parse_year(value: &str) -> Result<Option<u16>> {
    let year = parse_number::<u16>(value)?;
    match year {
        None | Some(0) => Ok(None),
        Some(1900..=2200) => Ok(year),
        Some(other) => bail!("{other} is outside the supported year-manufactured range"),
    }
}

fn validate_snapshot_date(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        bail!("FAA snapshot date must use YYYY-MM-DD");
    }
    let year: u16 = value[0..4].parse()?;
    let month: u8 = value[5..7].parse()?;
    let day: u8 = value[8..10].parse()?;
    if !(1900..=2200).contains(&year) || !(1..=12).contains(&month) || day == 0 {
        bail!("FAA snapshot date is outside the supported range");
    }
    let maximum_day = match month {
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day > maximum_day {
        bail!("FAA snapshot date has an invalid day for its month");
    }
    Ok(())
}

fn validate_faa_source_url(value: &str) -> Result<()> {
    let url = Url::parse(value).context("FAA source URL is invalid")?;
    let domain = url
        .domain()
        .context("FAA source URL must have a domain")?
        .to_ascii_lowercase();
    if url.scheme() != "https" || !(domain == "faa.gov" || domain.ends_with(".faa.gov")) {
        bail!("FAA source URL must be an official HTTPS faa.gov URL");
    }
    Ok(())
}

fn normalize_digest(value: &str, label: &str) -> Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.len() != 64
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("FAA {label} SHA-256 must contain exactly 64 hexadecimal characters");
    }
    Ok(normalized)
}

fn normalize_targets<I, S>(target_n_numbers: I) -> Result<BTreeSet<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let targets = target_n_numbers
        .into_iter()
        .map(|target| {
            let target = target.as_ref();
            normalize_n_number(target)
                .with_context(|| format!("FAA import target {target:?} is not a valid N-number"))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if targets.is_empty() {
        bail!("FAA import requires at least one target N-number");
    }
    Ok(targets)
}

fn target_set_digest(targets: &BTreeSet<String>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"aircost-faa-target-set-v1\0");
    for target in targets {
        hash_manifest_value(&mut digest, target);
    }
    format!("{:x}", digest.finalize())
}

fn source_manifest_digest(metadata: &ReleaseMetadata, members: [&MemberProvenance; 3]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"aircost-faa-source-manifest-v1\0");
    for value in [
        metadata.snapshot_date.as_str(),
        metadata.source_url.as_str(),
        metadata.archive_sha256.as_str(),
    ] {
        hash_manifest_value(&mut digest, value);
    }
    for member in members {
        hash_manifest_value(&mut digest, &member.member_name);
        hash_manifest_value(&mut digest, &member.sha256);
    }
    format!("{:x}", digest.finalize())
}

fn hash_manifest_value(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

fn logical_record_digest(record: &StringRecord) -> String {
    let mut digest = Sha256::new();
    digest.update(b"aircost-faa-master-logical-record-v1\0");
    digest.update((record.len() as u64).to_be_bytes());
    for field in record {
        // Preserve padding and source field order. Length prefixes make the
        // digest independent of separator escaping while remaining unambiguous.
        hash_manifest_value(&mut digest, field);
    }
    format!("{:x}", digest.finalize())
}

struct DigestReader<R> {
    inner: R,
    digest: Sha256,
}

impl<R> DigestReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
        }
    }

    fn finalize(self) -> String {
        format!("{:x}", self.digest.finalize())
    }
}

impl<R: Read> Read for DigestReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.digest.update(&buffer[..count]);
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::{Cursor, Write};
    use std::rc::Rc;

    use super::*;
    use tempfile::tempdir;
    use zip::write::{ExtendedFileOptions, FileOptions, SimpleFileOptions};
    use zip::ZipWriter;

    const MASTER: &str = "\u{feff}N-NUMBER,SERIAL NUMBER,MFR MDL CODE,ENG MFR MDL,YEAR MFR,NAME,STREET,MODE S CODE\n123AB, 182-01234 ,2072738,41528,2006,PRIVATE OWNER,SECRET ADDRESS,50000000\n456,ABC-99,0001234,00001,0000,ANOTHER OWNER,ANOTHER ADDRESS,50000001\n";
    const AIRCRAFT: &str = "\u{feff}CODE,MFR,MODEL,TYPE-ACFT,TYPE-ENG,AC-CAT,BUILD-CERT-IND,NO-ENG,NO-SEATS,AC-WEIGHT,SPEED,TC-DATA-SHEET,TC-DATA-HOLDER\n2072738,CESSNA AIRCRAFT CO,182T,4,1,1,0,01,004,CLASS 1,0145,3A13,TEXTRON AVIATION INC\n0001234,EXAMPLE,MODEL,4,1,1,1,01,002,CLASS 1,0100,,\n";
    const ENGINE: &str = "\u{feff}CODE,MFR,MODEL,TYPE,HORSEPOWER,THRUST\n41528,LYCOMING,IO-540-AB1A5,1,00230,000000\n00001,NONE,NONE,0,00000,000000\n";

    fn metadata() -> ReleaseMetadata {
        ReleaseMetadata::official("2026-07-20", "A".repeat(64))
    }

    fn archive(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        let mut archive = writer.finish().unwrap();
        archive.set_position(0);
        archive
    }

    fn official_archive() -> Cursor<Vec<u8>> {
        archive(&[
            (MASTER_MEMBER_NAME, MASTER.as_bytes()),
            (AIRCRAFT_MEMBER_NAME, AIRCRAFT.as_bytes()),
            (ENGINE_MEMBER_NAME, ENGINE.as_bytes()),
            ("README.txt", b"FAA release fixture"),
        ])
    }

    fn archive_with_extra_field() -> Cursor<Vec<u8>> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let mut options = FileOptions::<ExtendedFileOptions>::default()
            .compression_method(CompressionMethod::Stored);
        options.add_extra_data(0xcafe, b"fixture", false).unwrap();
        for (name, bytes) in [
            (MASTER_MEMBER_NAME, MASTER.as_bytes()),
            (AIRCRAFT_MEMBER_NAME, AIRCRAFT.as_bytes()),
            (ENGINE_MEMBER_NAME, ENGINE.as_bytes()),
        ] {
            writer.start_file(name, options.clone()).unwrap();
            writer.write_all(bytes).unwrap();
        }
        let mut archive = writer.finish().unwrap();
        archive.set_position(0);
        archive
    }

    struct SwitchingAfterEofReader {
        primary: Cursor<Vec<u8>>,
        replacement: Cursor<Vec<u8>>,
        primary_eof_seen: bool,
        reads_after_primary_eof: Rc<Cell<usize>>,
    }

    impl Read for SwitchingAfterEofReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.primary_eof_seen {
                self.reads_after_primary_eof
                    .set(self.reads_after_primary_eof.get() + 1);
                return self.replacement.read(buffer);
            }
            let count = self.primary.read(buffer)?;
            if count == 0 {
                self.primary_eof_seen = true;
            }
            Ok(count)
        }
    }

    struct GrowingReader;

    impl Read for GrowingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            buffer.fill(b'x');
            Ok(buffer.len())
        }
    }

    fn mark_all_entries_encrypted(bytes: &mut [u8]) {
        for index in 0..bytes.len().saturating_sub(10) {
            let flag_offset = if bytes[index..].starts_with(b"PK\x03\x04") {
                Some(index + 6)
            } else if bytes[index..].starts_with(b"PK\x01\x02") {
                Some(index + 8)
            } else {
                None
            };
            if let Some(flag_offset) = flag_offset {
                let flags = u16::from_le_bytes([bytes[flag_offset], bytes[flag_offset + 1]]) | 1;
                bytes[flag_offset..flag_offset + 2].copy_from_slice(&flags.to_le_bytes());
            }
        }
    }

    fn declare_first_member_oversized(bytes: &mut [u8]) {
        let oversized = u32::try_from(MAX_MASTER_BYTES + 1).unwrap().to_le_bytes();
        let local = bytes
            .windows(4)
            .position(|window| window == b"PK\x03\x04")
            .unwrap();
        bytes[local + 22..local + 26].copy_from_slice(&oversized);
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .unwrap();
        bytes[central + 24..central + 28].copy_from_slice(&oversized);
    }

    fn rename_archive_members(bytes: &mut [u8], from: &[u8], to: &[u8]) {
        assert_eq!(from.len(), to.len());
        for offset in 0..=bytes.len().saturating_sub(from.len()) {
            if &bytes[offset..offset + from.len()] == from {
                bytes[offset..offset + to.len()].copy_from_slice(to);
            }
        }
    }

    fn end_of_central_directory_offset(bytes: &[u8]) -> usize {
        bytes
            .windows(4)
            .rposition(|window| window == END_OF_CENTRAL_DIRECTORY_SIGNATURE.to_le_bytes())
            .unwrap()
    }

    fn set_entry_dos_date(bytes: &mut [u8], name: &str, local: Option<u16>, central: Option<u16>) {
        let name = name.as_bytes();
        let mut offset = 0_usize;
        while offset + 4 <= bytes.len() {
            let signature = little_u32(&bytes[offset..offset + 4]);
            let (name_length_offset, name_offset, date_offset, replacement) = match signature {
                LOCAL_FILE_HEADER_SIGNATURE => (offset + 26, offset + 30, offset + 12, local),
                CENTRAL_DIRECTORY_ENTRY_SIGNATURE => {
                    (offset + 28, offset + 46, offset + 14, central)
                }
                _ => {
                    offset += 1;
                    continue;
                }
            };
            if name_length_offset + 2 > bytes.len() {
                break;
            }
            let length = usize::from(little_u16(
                &bytes[name_length_offset..name_length_offset + 2],
            ));
            if name_offset + length <= bytes.len()
                && &bytes[name_offset..name_offset + length] == name
            {
                if let Some(replacement) = replacement {
                    bytes[date_offset..date_offset + 2].copy_from_slice(&replacement.to_le_bytes());
                }
            }
            offset += 4;
        }
    }

    #[test]
    fn archive_parser_binds_complete_zip_and_exact_member_hashes() {
        let archive = official_archive();
        let expected_archive_sha256 = format!("{:x}", Sha256::digest(archive.get_ref()));
        let release = parse_release_archive(archive, ["N123AB", "N456", "N999ZZ"]).unwrap();

        assert_eq!(release.metadata.archive_sha256, expected_archive_sha256);
        assert_eq!(release.metadata.snapshot_date, "1980-01-01");
        assert_eq!(
            release.master.sha256,
            format!("{:x}", Sha256::digest(MASTER.as_bytes()))
        );
        assert_eq!(
            release.aircraft_reference.sha256,
            format!("{:x}", Sha256::digest(AIRCRAFT.as_bytes()))
        );
        assert_eq!(
            release.engine_reference.sha256,
            format!("{:x}", Sha256::digest(ENGINE.as_bytes()))
        );
        assert_eq!(release.aircraft.len(), 2);
        assert_eq!(release.coverage.len(), 3);
    }

    #[test]
    fn archive_parser_rejects_required_date_disagreement_and_invalid_dates() {
        let mut mismatched = official_archive().into_inner();
        set_entry_dos_date(&mut mismatched, AIRCRAFT_MEMBER_NAME, Some(34), Some(34));
        let error = parse_release_archive(Cursor::new(mismatched), ["N123AB"]).unwrap_err();
        assert!(error.to_string().contains("do not share one release date"));

        let mut malformed = official_archive().into_inner();
        set_entry_dos_date(&mut malformed, MASTER_MEMBER_NAME, None, Some(0));
        let error = parse_release_archive(Cursor::new(malformed), ["N123AB"]).unwrap_err();
        assert!(error.to_string().contains("timestamp is invalid"));
    }

    #[test]
    fn archive_parser_rejects_local_central_timestamp_disagreement() {
        let mut mismatched = official_archive().into_inner();
        set_entry_dos_date(&mut mismatched, MASTER_MEMBER_NAME, Some(34), None);
        let error = parse_release_archive(Cursor::new(mismatched), ["N123AB"]).unwrap_err();
        assert!(error
            .to_string()
            .contains("disagrees with its central entry"));
    }

    #[test]
    #[ignore = "requires the locally cached 2026-08-19 FAA release"]
    fn cached_official_archive_derives_intrinsic_release_date() {
        let archive = File::open("/tmp/aircost-faa-20260819-ReleasableAircraft.zip").unwrap();
        let release = parse_release_archive(archive, ["N182KW"]).unwrap();
        assert_eq!(release.metadata.snapshot_date, "2026-08-18");
    }

    #[test]
    fn archive_parser_snapshots_the_caller_once_before_parsing() {
        let reads_after_primary_eof = Rc::new(Cell::new(0));
        let reader = SwitchingAfterEofReader {
            primary: official_archive(),
            replacement: archive(&[("replacement.txt", b"not the accepted archive")]),
            primary_eof_seen: false,
            reads_after_primary_eof: Rc::clone(&reads_after_primary_eof),
        };
        let release = parse_release_archive(reader, ["N123AB"]).unwrap();
        assert_eq!(release.aircraft.len(), 1);
        assert_eq!(reads_after_primary_eof.get(), 0);
    }

    #[test]
    fn archive_snapshot_enforces_streamed_size_and_cleans_up_failures() {
        let temporary_directory = tempdir().unwrap();
        let error =
            parse_release_archive_in(GrowingReader, ["N123AB"], temporary_directory.path(), 64)
                .unwrap_err();
        assert!(error.to_string().contains("maximum accepted size"));
        assert_eq!(temporary_directory.path().read_dir().unwrap().count(), 0);

        let error = parse_release_archive_in(
            Cursor::new(b"not a ZIP"),
            ["N123AB"],
            temporary_directory.path(),
            64,
        )
        .unwrap_err();
        assert!(error.to_string().contains("too short"));
        assert_eq!(temporary_directory.path().read_dir().unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn archive_snapshot_file_is_private_and_anonymous() {
        use std::os::unix::fs::PermissionsExt;

        let temporary_directory = tempdir().unwrap();
        let (snapshot, _) = snapshot_archive(
            Cursor::new(official_archive().into_inner()),
            temporary_directory.path(),
            MAX_ARCHIVE_BYTES,
        )
        .unwrap();
        assert_eq!(snapshot.metadata().unwrap().permissions().mode() & 0o077, 0);
        assert_eq!(temporary_directory.path().read_dir().unwrap().count(), 0);
    }

    #[test]
    fn archive_parser_rejects_missing_and_duplicate_required_root_members() {
        let missing = archive(&[
            (MASTER_MEMBER_NAME, MASTER.as_bytes()),
            (AIRCRAFT_MEMBER_NAME, AIRCRAFT.as_bytes()),
        ]);
        assert!(parse_release_archive(missing, ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("missing root ENGINE.txt"));

        let mut duplicate = archive(&[
            (MASTER_MEMBER_NAME, MASTER.as_bytes()),
            ("MASTEX.txt", MASTER.as_bytes()),
            (AIRCRAFT_MEMBER_NAME, AIRCRAFT.as_bytes()),
            (ENGINE_MEMBER_NAME, ENGINE.as_bytes()),
        ]);
        rename_archive_members(duplicate.get_mut(), b"MASTEX.txt", b"MASTER.txt");
        assert!(parse_release_archive(duplicate, ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("duplicate member \"MASTER.txt\""));

        let nested_only = archive(&[
            ("nested/MASTER.txt", MASTER.as_bytes()),
            (AIRCRAFT_MEMBER_NAME, AIRCRAFT.as_bytes()),
            (ENGINE_MEMBER_NAME, ENGINE.as_bytes()),
        ]);
        assert!(parse_release_archive(nested_only, ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("missing root MASTER.txt"));
    }

    #[test]
    fn archive_parser_rejects_encryption_traversal_and_oversized_members() {
        let mut encrypted = official_archive().into_inner();
        mark_all_entries_encrypted(&mut encrypted);
        assert!(parse_release_archive(Cursor::new(encrypted), ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("is encrypted"));

        let traversal = archive(&[
            (MASTER_MEMBER_NAME, MASTER.as_bytes()),
            (AIRCRAFT_MEMBER_NAME, AIRCRAFT.as_bytes()),
            (ENGINE_MEMBER_NAME, ENGINE.as_bytes()),
            ("../outside.txt", b"unsafe"),
        ]);
        assert!(parse_release_archive(traversal, ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("unsafe member path"));

        let mut oversized = official_archive().into_inner();
        declare_first_member_oversized(&mut oversized);
        assert!(parse_release_archive(Cursor::new(oversized), ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("accepted range"));
    }

    #[test]
    fn archive_parser_rejects_multi_disk_and_zip64_archives() {
        assert!(parse_release_archive(Cursor::new(b"not a zip"), ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("too short"));

        let mut multi_disk = official_archive().into_inner();
        let eocd = end_of_central_directory_offset(&multi_disk);
        multi_disk[eocd + 4..eocd + 6].copy_from_slice(&1_u16.to_le_bytes());
        assert!(parse_release_archive(Cursor::new(multi_disk), ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("single-disk"));

        let mut zip64 = official_archive().into_inner();
        let eocd = end_of_central_directory_offset(&zip64);
        zip64[eocd + 10..eocd + 12].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(parse_release_archive(Cursor::new(zip64), ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("ZIP64"));

        let mut entry_zip64 = official_archive().into_inner();
        let central = entry_zip64
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .unwrap();
        entry_zip64[central + 20..central + 24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_release_archive(Cursor::new(entry_zip64), ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("ZIP64 per-entry"));

        let mut extra_zip64 = archive_with_extra_field().into_inner();
        for offset in 0..extra_zip64.len().saturating_sub(1) {
            if extra_zip64[offset..offset + 2] == 0xcafe_u16.to_le_bytes() {
                extra_zip64[offset..offset + 2]
                    .copy_from_slice(&ZIP64_EXTRA_FIELD_ID.to_le_bytes());
            }
        }
        assert!(parse_release_archive(Cursor::new(extra_zip64), ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("ZIP64 per-entry extra"));
    }

    #[test]
    fn archive_parser_rejects_local_central_mismatch_and_bad_offsets() {
        let mut mismatched_name = official_archive().into_inner();
        let local = mismatched_name
            .windows(4)
            .position(|window| window == b"PK\x03\x04")
            .unwrap();
        mismatched_name[local + LOCAL_FILE_HEADER_BYTES] = b'X';
        assert!(
            parse_release_archive(Cursor::new(mismatched_name), ["N123AB"])
                .unwrap_err()
                .to_string()
                .contains("name disagrees")
        );

        let mut mismatched_flags = official_archive().into_inner();
        let local = mismatched_flags
            .windows(4)
            .position(|window| window == b"PK\x03\x04")
            .unwrap();
        mismatched_flags[local + 6..local + 8].copy_from_slice(&8_u16.to_le_bytes());
        assert!(
            parse_release_archive(Cursor::new(mismatched_flags), ["N123AB"])
                .unwrap_err()
                .to_string()
                .contains("disagrees with its central entry")
        );

        let mut bad_offset = official_archive().into_inner();
        let central = bad_offset
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .unwrap();
        let eocd = end_of_central_directory_offset(&bad_offset);
        bad_offset[central + 42..central + 46]
            .copy_from_slice(&u32::try_from(eocd).unwrap().to_le_bytes());
        assert!(parse_release_archive(Cursor::new(bad_offset), ["N123AB"])
            .unwrap_err()
            .to_string()
            .contains("outside file-data bounds"));
    }

    #[test]
    fn unsigned_descriptor_crc_equal_to_signature_is_not_misparsed() {
        let entry = CentralDirectoryEntry {
            raw_name: b"fixture".to_vec(),
            flags: 1 << 3,
            compression: 0,
            modification_time: 0,
            modification_date: 33,
            crc32: DATA_DESCRIPTOR_SIGNATURE,
            compressed_size: 4,
            uncompressed_size: 4,
            local_header_offset: 0,
        };
        let mut descriptor = Vec::new();
        descriptor.extend_from_slice(&DATA_DESCRIPTOR_SIGNATURE.to_le_bytes());
        descriptor.extend_from_slice(&4_u32.to_le_bytes());
        descriptor.extend_from_slice(&4_u32.to_le_bytes());
        let end = validate_data_descriptor(
            &mut Cursor::new(descriptor),
            0,
            0,
            u64::try_from(DATA_DESCRIPTOR_BYTES_WITHOUT_SIGNATURE).unwrap(),
            &entry,
        )
        .unwrap();
        assert_eq!(end, 12);
    }

    #[test]
    fn member_reader_enforces_actual_uncompressed_size() {
        let mut exact = SizeLimitedReader::new(Cursor::new(b"abc"), 3, "fixture.txt");
        let mut contents = Vec::new();
        exact.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"abc");

        let mut oversized = SizeLimitedReader::new(Cursor::new(b"abcd"), 3, "fixture.txt");
        let error = oversized.read_to_end(&mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn parses_only_safe_source_projections_and_preserves_codes() {
        let release = ReleaseFixtureBuilder::from_csv(
            metadata(),
            Cursor::new(MASTER),
            Cursor::new(AIRCRAFT),
            Cursor::new(ENGINE),
            ["N123AB", "N456", "N999ZZ"],
        )
        .unwrap();

        assert_eq!(release.aircraft.len(), 2);
        assert_eq!(release.coverage.len(), 3);
        assert!(release.coverage[0].matched);
        assert!(release.coverage[1].matched);
        assert!(!release.coverage[2].matched);
        assert_eq!(release.aircraft[0].n_number, "N123AB");
        assert_eq!(release.aircraft[0].aircraft_code, "2072738");
        assert_eq!(release.aircraft[0].year_manufactured, Some(2006));
        assert_eq!(release.aircraft[0].source_record_sha256.len(), 64);
        assert_eq!(release.aircraft[1].aircraft_code, "0001234");
        assert_eq!(release.aircraft[1].engine_code.as_deref(), Some("00001"));
        assert_eq!(release.aircraft[1].year_manufactured, None);
        assert_eq!(release.aircraft_references[1].aircraft_code, "0001234");
        assert_eq!(release.engine_references[1].engine_code, "00001");
        assert_eq!(release.metadata.archive_sha256, "a".repeat(64));
        assert_eq!(release.master.sha256.len(), 64);
        assert_eq!(release.source_manifest_sha256.len(), 64);
        assert_eq!(release.target_set_sha256.len(), 64);

        // These PII fixture values are consumed by the CSV reader but have no
        // field in the safe in-memory or persisted representation.
        let debug = format!("{release:?}");
        assert!(!debug.contains("PRIVATE OWNER"));
        assert!(!debug.contains("SECRET ADDRESS"));
        assert!(!debug.contains("50000000"));
    }

    #[test]
    fn rejects_non_faa_provenance_and_duplicate_registrations() {
        let mut invalid_metadata = metadata();
        invalid_metadata.source_url = "https://example.com/aircraft.zip".to_string();
        assert!(ReleaseFixtureBuilder::from_csv(
            invalid_metadata,
            Cursor::new(MASTER),
            Cursor::new(AIRCRAFT),
            Cursor::new(ENGINE),
            ["N123AB"],
        )
        .unwrap_err()
        .to_string()
        .contains("official HTTPS faa.gov"));

        let duplicate_master = format!("{MASTER}n-123ab,OTHER,2072738,41528,2006,X,Y,Z\n");
        assert!(ReleaseFixtureBuilder::from_csv(
            metadata(),
            Cursor::new(duplicate_master),
            Cursor::new(AIRCRAFT),
            Cursor::new(ENGINE),
            ["N123AB"],
        )
        .unwrap_err()
        .to_string()
        .contains("duplicate normalized N-number"));
    }

    #[test]
    fn rejects_invalid_snapshot_date_and_missing_required_header() {
        let mut invalid_date = metadata();
        invalid_date.snapshot_date = "2026-02-30".to_string();
        assert!(ReleaseFixtureBuilder::from_csv(
            invalid_date,
            Cursor::new(MASTER),
            Cursor::new(AIRCRAFT),
            Cursor::new(ENGINE),
            ["N123AB"],
        )
        .is_err());

        let invalid_master = "N-NUMBER,SERIAL NUMBER\n123,ABC\n";
        assert!(ReleaseFixtureBuilder::from_csv(
            metadata(),
            Cursor::new(invalid_master),
            Cursor::new(AIRCRAFT),
            Cursor::new(ENGINE),
            ["N123AB"],
        )
        .unwrap_err()
        .to_string()
        .contains("MFR MDL CODE"));
    }
}
