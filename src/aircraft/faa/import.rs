use std::collections::BTreeSet;
use std::io::{self, Read};

use anyhow::{bail, Context, Result};
use csv::{Reader, ReaderBuilder, StringRecord};
use sha2::{Digest, Sha256};
use url::Url;

use super::{
    normalize_n_number, normalize_serial_key, AircraftRecord, AircraftReference, EngineReference,
    MemberProvenance, Release, ReleaseMetadata, TargetCoverage, AIRCRAFT_MEMBER_NAME,
    ENGINE_MEMBER_NAME, MASTER_MEMBER_NAME,
};

/// Extracted FAA release members. ZIP extraction is intentionally left to the
/// caller so this domain never needs to ingest unrelated registrant files.
pub struct ReleaseReaders<M, A, E> {
    pub master: M,
    pub aircraft_reference: A,
    pub engine_reference: E,
}

impl<M, A, E> ReleaseReaders<M, A, E> {
    pub fn new(master: M, aircraft_reference: A, engine_reference: E) -> Self {
        Self {
            master,
            aircraft_reference,
            engine_reference,
        }
    }
}

/// Parses exactly the three non-PII inputs needed for registry grounding.
///
/// Member digests are computed while parsing. The caller must provide the
/// digest of the original archive, retaining a complete provenance chain even
/// though the archive itself is not stored in the database.
pub fn parse_release<M, A, E, I, S>(
    mut metadata: ReleaseMetadata,
    readers: ReleaseReaders<M, A, E>,
    target_n_numbers: I,
) -> Result<Release>
where
    M: Read,
    A: Read,
    E: Read,
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    validate_snapshot_date(&metadata.snapshot_date)?;
    validate_faa_source_url(&metadata.source_url)?;
    metadata.archive_sha256 = normalize_digest(&metadata.archive_sha256, "archive")?;

    let targets = normalize_targets(target_n_numbers)?;
    let target_set_sha256 = target_set_digest(&targets);
    let (aircraft, coverage, master_sha256) = parse_master(readers.master, &targets)?;
    let aircraft_codes = aircraft
        .iter()
        .map(|record| record.aircraft_code.as_str())
        .collect::<BTreeSet<_>>();
    let engine_codes = aircraft
        .iter()
        .filter_map(|record| record.engine_code.as_deref())
        .collect::<BTreeSet<_>>();
    let (aircraft_references, aircraft_sha256) =
        parse_aircraft_references(readers.aircraft_reference, &aircraft_codes)?;
    let (engine_references, engine_sha256) =
        parse_engine_references(readers.engine_reference, &engine_codes)?;

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

    Ok(Release {
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
    })
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
    use std::io::Cursor;

    use super::*;

    const MASTER: &str = "\u{feff}N-NUMBER,SERIAL NUMBER,MFR MDL CODE,ENG MFR MDL,YEAR MFR,NAME,STREET,MODE S CODE\n123AB, 182-01234 ,2072738,41528,2006,PRIVATE OWNER,SECRET ADDRESS,50000000\n456,ABC-99,0001234,00001,0000,ANOTHER OWNER,ANOTHER ADDRESS,50000001\n";
    const AIRCRAFT: &str = "\u{feff}CODE,MFR,MODEL,TYPE-ACFT,TYPE-ENG,AC-CAT,BUILD-CERT-IND,NO-ENG,NO-SEATS,AC-WEIGHT,SPEED,TC-DATA-SHEET,TC-DATA-HOLDER\n2072738,CESSNA AIRCRAFT CO,182T,4,1,1,0,01,004,CLASS 1,0145,3A13,TEXTRON AVIATION INC\n0001234,EXAMPLE,MODEL,4,1,1,1,01,002,CLASS 1,0100,,\n";
    const ENGINE: &str = "\u{feff}CODE,MFR,MODEL,TYPE,HORSEPOWER,THRUST\n41528,LYCOMING,IO-540-AB1A5,1,00230,000000\n00001,NONE,NONE,0,00000,000000\n";

    fn metadata() -> ReleaseMetadata {
        ReleaseMetadata::official("2026-07-20", "A".repeat(64))
    }

    #[test]
    fn parses_only_safe_source_projections_and_preserves_codes() {
        let release = parse_release(
            metadata(),
            ReleaseReaders::new(
                Cursor::new(MASTER),
                Cursor::new(AIRCRAFT),
                Cursor::new(ENGINE),
            ),
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
        assert!(parse_release(
            invalid_metadata,
            ReleaseReaders::new(
                Cursor::new(MASTER),
                Cursor::new(AIRCRAFT),
                Cursor::new(ENGINE)
            ),
            ["N123AB"]
        )
        .unwrap_err()
        .to_string()
        .contains("official HTTPS faa.gov"));

        let duplicate_master = format!("{MASTER}n-123ab,OTHER,2072738,41528,2006,X,Y,Z\n");
        assert!(parse_release(
            metadata(),
            ReleaseReaders::new(
                Cursor::new(duplicate_master),
                Cursor::new(AIRCRAFT),
                Cursor::new(ENGINE)
            ),
            ["N123AB"]
        )
        .unwrap_err()
        .to_string()
        .contains("duplicate normalized N-number"));
    }

    #[test]
    fn rejects_invalid_snapshot_date_and_missing_required_header() {
        let mut invalid_date = metadata();
        invalid_date.snapshot_date = "2026-02-30".to_string();
        assert!(parse_release(
            invalid_date,
            ReleaseReaders::new(
                Cursor::new(MASTER),
                Cursor::new(AIRCRAFT),
                Cursor::new(ENGINE)
            ),
            ["N123AB"]
        )
        .is_err());

        let invalid_master = "N-NUMBER,SERIAL NUMBER\n123,ABC\n";
        assert!(parse_release(
            metadata(),
            ReleaseReaders::new(
                Cursor::new(invalid_master),
                Cursor::new(AIRCRAFT),
                Cursor::new(ENGINE)
            ),
            ["N123AB"]
        )
        .unwrap_err()
        .to_string()
        .contains("MFR MDL CODE"));
    }
}
