//! VU data download handling: CRC verification, VRN extraction, and storage.
//!
//! The device does `RequestUpload`/`TransferData`/`RequestTransferExit`
//! entirely against the VU over its own CAN/UDS link — none of that touches
//! the physical company card, so there's nothing for this server to relay in
//! real time. What lands here is the finished result: this server represents
//! the company back-office that actually wants the downloaded tachograph
//! data, so the device hands over the complete file (chunked, since it can
//! exceed our wire format's 65535-byte single-message cap) for us to verify
//! and store.

use std::path::PathBuf;

use crate::config::{get_from_cache, CacheSection};

/// Gen1 `Overview` block layout (fixed offsets, verified against a real
/// open-source tachograph parser and independently against documented Gen1
/// certificate sizes — see the plan behind this module for sources):
///
/// | Field | Offset | Length |
/// |---|---|---|
/// | MemberStateCertificate | 0 | 194 |
/// | VuCertificate | 194 | 194 |
/// | VehicleIdentificationNumber | 388 | 17 |
/// | VehicleRegistrationIdentification.nation | 405 | 1 |
/// | VehicleRegistrationIdentification.codePage | 406 | 1 |
/// | VehicleRegistrationIdentification.vehicleRegNumber (VRN) | 407 | 13 |
fn extract_vrn_gen1(data: &[u8]) -> Option<String> {
    let vrn_bytes = data.get(407..420)?;
    Some(decode_padded_text(vrn_bytes))
}

/// Gen2 downloads are a sequence of TREP blocks (Overview, EventsAndFaults,
/// DetailedSpeed, TechnicalData, ...), each starting with a 2-byte separator
/// `0x76 <trep_id>` — confirmed byte-for-byte against a real multi-block
/// capture: `76 21` before Overview, `76 23` before EventsAndFaults, `76 24`
/// before DetailedSpeed, matching the well-known "Gen2 TREP = Gen1 TREP with
/// 0x20 added" convention (0x21=Overview, 0x22=Activities, 0x23=EventsAndFaults,
/// 0x24=DetailedSpeed, 0x25=TechnicalData). This was originally mistaken for a
/// one-off 2-byte preamble before treating it as a recurring per-block
/// separator — the giveaway was the exact same `0x76 <trep_id>` shape
/// reappearing at every point a new block should start.
///
/// Within a block, the content is a sequence of self-describing
/// `RecordArray`s, each `recordType(1) + recordSize(2, u16 BE) +
/// noOfRecords(2, u16 BE)` header followed by `recordSize * noOfRecords`
/// bytes of data.
///
/// Checking for the separator only ever happens at a position reached by
/// fully consuming a prior record array's *declared* length (or the very
/// start) — never by scanning arbitrary bytes for a 0x76 value. That matters:
/// 0x76 is just an ordinary byte value that legitimately turns up inside
/// certificate/signature data (confirmed in the same capture), so checking
/// for it anywhere other than an actual block boundary would misfire.
fn skip_trep_separator(data: &[u8], offset: usize) -> usize {
    if data.get(offset) == Some(&0x76) && offset + 1 < data.len() {
        offset + 2
    } else {
        offset
    }
}

const VRN_LENGTH: usize = 13;

/// Overview block field order: MemberStateCertificateRecordArray,
/// VuCertificateRecordArray, VehicleIdentificationNumberRecordArray,
/// VehicleRegistrationNumberRecordArray (the one we want) — walk/skip the
/// first three generically using their own declared sizes, then read the
/// fourth using its own declared record size.
///
/// The VehicleRegistrationNumber record is `nation(1) + vehicleRegNumber(13)`,
/// 14 bytes, in Gen2 downloads, but `nation(1) + codePage(1) +
/// vehicleRegNumber(13)`, 15 bytes, in Gen2V2 ones — both confirmed against real
/// captures. Either way the registration is the record's last 13 bytes.
fn extract_vrn_gen2(data: &[u8]) -> Option<String> {
    let mut offset = skip_trep_separator(data, 0);

    // Skip MemberStateCertificateRecordArray, VuCertificateRecordArray,
    // VehicleIdentificationNumberRecordArray.
    for _ in 0..3 {
        offset = skip_record_array(data, offset)?;
    }

    // VehicleRegistrationNumberRecordArray — read its header, then its first
    // record's content (nation(1) + vehicleRegNumber, using the header's own
    // declared record_size rather than hardcoding 14).
    let header = read_record_array_header(data, offset)?;
    let record_start = offset + 5;
    if header.no_of_records == 0 || (header.record_size as usize) < 1 + VRN_LENGTH {
        return None;
    }
    let record = data.get(record_start..record_start + header.record_size as usize)?;
    Some(decode_padded_text(&record[record.len() - VRN_LENGTH..]))
}

struct RecordArrayHeader {
    record_size: u16,
    no_of_records: u16,
}

fn read_record_array_header(data: &[u8], offset: usize) -> Option<RecordArrayHeader> {
    let header = data.get(offset..offset + 5)?;
    Some(RecordArrayHeader {
        record_size: u16::from_be_bytes([header[1], header[2]]),
        no_of_records: u16::from_be_bytes([header[3], header[4]]),
    })
}

/// Returns the offset just past this record array (header + all its records).
fn skip_record_array(data: &[u8], offset: usize) -> Option<usize> {
    let header = read_record_array_header(data, offset)?;
    let data_len = header.record_size as usize * header.no_of_records as usize;
    let next = offset + 5 + data_len;
    if next > data.len() {
        return None;
    }
    Some(next)
}

/// Walks the *entire* buffer as a sequence of TREP blocks (each starting with
/// a `skip_trep_separator`-recognized marker) containing record arrays,
/// verifying every declared block actually fits with nothing left over and
/// nothing overrunning. This is a purely structural check — it confirms sizes
/// are internally consistent, not that field contents mean anything — but
/// that's exactly what it's for: `extract_vrn` only walks as far as
/// Overview's fourth record array and would happily return a VRN even if
/// something later in the file is truncated or corrupt.
///
/// Returns `Ok(total_record_arrays)` if the walk lands exactly on the end of
/// the buffer, or `Err(offset)` with the byte offset where it stopped making
/// sense — either a header that doesn't fit in the remaining bytes, a
/// declared size that would run past the end of the buffer, or leftover
/// trailing bytes too short to be another header. Section (TREP) and record
/// types are never inspected, so sections or record types it hasn't seen
/// before pass as long as their declared sizes are consistent.
///
/// Diagnostic only: the result is logged and tagged in the saved filename
/// (`unparsed_at_<offset>`) but never fails a download — the CRC is the sole
/// pass/fail check (see `socket_server`'s `DownloadComplete` handling).
///
/// Gen1-only note: there's no equivalent check for generation 1 — its
/// certificates/fields are fixed-offset, not self-describing, so there's
/// nothing declared in the byte stream itself to check sizes against.
pub fn validate_gen2_structure(data: &[u8]) -> Result<usize, usize> {
    let mut offset = skip_trep_separator(data, 0);
    let mut count = 0usize;
    while offset < data.len() {
        offset = skip_trep_separator(data, offset);
        if offset >= data.len() {
            break;
        }
        let start_of_this_block = offset;
        match skip_record_array(data, offset) {
            Some(next) => {
                offset = next;
                count += 1;
            }
            None => return Err(start_of_this_block),
        }
    }

    if offset == data.len() {
        Ok(count)
    } else {
        // Shouldn't happen given skip_record_array's own bounds check, but
        // defensive: never claim success on an inexact finish.
        Err(offset)
    }
}

/// Which file a `DownloadChunk`'s data belongs in. From firmware D7.0.83 every
/// chunk starts with one of these bytes; before that it's all the VU's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Vu,
    /// The driver slot's card, downloaded through the VU (`data_types::DRIVER_CARD`).
    DriverCard,
}

impl Section {
    /// `None` for anything else, including the co-driver slot's 0x02, which the
    /// firmware reserves but doesn't send.
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x00 => Some(Self::Vu),
            0x01 => Some(Self::DriverCard),
            _ => None,
        }
    }
}

/// A download as it arrives: the VU's data and the driver card's, saved as
/// separate files, and the order it arrived in, for the CRC32 the device computes
/// over all of it in the order it was sent (without the section bytes) and for
/// cutting it back to where a resumed download carries on from.
#[derive(Debug, Default)]
pub struct DownloadData {
    pub vu: Vec<u8>,
    pub driver_card: Vec<u8>,
    /// Runs of consecutive bytes for one section, in the order they arrived.
    runs: Vec<(Section, usize)>,
}

impl DownloadData {
    fn section_mut(&mut self, section: Section) -> &mut Vec<u8> {
        match section {
            Section::Vu => &mut self.vu,
            Section::DriverCard => &mut self.driver_card,
        }
    }

    pub fn add(&mut self, section: Section, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.section_mut(section).extend_from_slice(data);
        match self.runs.last_mut() {
            Some((last, length)) if *last == section => *length += data.len(),
            _ => self.runs.push((section, data.len())),
        }
    }

    pub fn len(&self) -> usize {
        self.vu.len() + self.driver_card.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Keeps only the first `length` bytes, in the order they arrived. Does nothing
    /// if there are no more than that.
    pub fn truncate(&mut self, length: usize) {
        let mut kept = 0;
        let mut kept_runs = 0;
        let (mut vu, mut driver_card) = (0, 0);
        for (section, run) in self.runs.iter_mut() {
            if kept == length {
                break;
            }
            *run = (*run).min(length - kept);
            kept += *run;
            kept_runs += 1;
            match section {
                Section::Vu => vu += *run,
                Section::DriverCard => driver_card += *run,
            }
        }
        if kept < length {
            return; // There were no more than `length` bytes; nothing was cut.
        }
        self.runs.truncate(kept_runs);
        self.vu.truncate(vu);
        self.driver_card.truncate(driver_card);
    }

    pub fn crc32(&self) -> u32 {
        let mut crc = crc32fast::Hasher::new();
        let (mut vu, mut driver_card) = (0, 0);
        for &(section, run) in &self.runs {
            let (data, offset) = match section {
                Section::Vu => (&self.vu, &mut vu),
                Section::DriverCard => (&self.driver_card, &mut driver_card),
            };
            crc.update(&data[*offset..*offset + run]);
            *offset += run;
        }
        crc.finalize()
    }
}

/// The card's files in a driver card section. The device relays the section as
/// a TREP section like the VU's, so it starts with the `76 06` separator (once, not
/// per block), which isn't part of the card download storage format.
pub fn card_files(section: &[u8]) -> &[u8] {
    section.strip_prefix(&[0x76, 0x06]).unwrap_or(section)
}

/// One of a driver card section's files: `fileId(2) + type(1) + length(2, u16
/// BE)` and then `length` bytes, the card download storage format of Annex 1C
/// Appendix 7. The type's low bit is set for a signature rather than the file's
/// data. Returns the header and data, or `None` if they don't fit in `data`.
fn read_card_file(data: &[u8], offset: usize) -> Option<(&[u8], &[u8])> {
    let header = data.get(offset..offset + 5)?;
    let length = u16::from_be_bytes([header[3], header[4]]) as usize;
    Some((header, data.get(offset + 5..offset + 5 + length)?))
}

/// `validate_gen2_structure` for a driver card section: walks it as card files
/// (see `read_card_file`), `Ok(files)` if that lands exactly on the end, or
/// `Err(offset)` where it stopped making sense. Diagnostic only, likewise.
pub fn validate_card_structure(data: &[u8]) -> Result<usize, usize> {
    let mut offset = 0;
    let mut count = 0usize;
    while offset < data.len() {
        let (_, value) = read_card_file(data, offset).ok_or(offset)?;
        offset += 5 + value.len();
        count += 1;
    }
    Ok(count)
}

/// The card number from a driver card section's `EF_Identification` (file
/// 0x0520), whose data starts `cardIssuingMemberState(1) + cardNumber(16)`.
/// Best-effort, like `extract_vrn`.
pub fn extract_card_number(data: &[u8]) -> Option<String> {
    let mut offset = 0;
    while let Some((header, value)) = read_card_file(data, offset) {
        if header[..2] == [0x05, 0x20] && header[2] & 0x01 == 0 {
            let number = decode_padded_text(value.get(1..17)?);
            return (!number.is_empty()).then_some(number);
        }
        offset += 5 + value.len();
    }
    None
}

/// `vehicleRegNumber` is code-page-encoded text. Decoding as ISO-8859-1 (byte
/// value == Unicode code point) for any codePage value is a known
/// simplification — covers the common case without implementing every code
/// page table from the data dictionary. Trims trailing NUL/space padding.
fn decode_padded_text(bytes: &[u8]) -> String {
    let text: String = bytes.iter().map(|&b| b as char).collect();
    text.trim_end_matches(['\0', ' ']).to_string()
}

/// Strips characters that are invalid in filenames on common filesystems
/// (notably Windows, which this project targets first, and Windows clients of a
/// NAS's file share, which show a name with a control character in it only as a
/// mangled short name like `_4VKR0~G.DDD`). VRNs are arbitrary per-country
/// strings, so this is defensive rather than expected to trigger often.
fn sanitize_for_filename(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() && !"/\\:*?\"<>|".contains(*c)).collect()
}

/// Best-effort — returns `None` on any parse failure (short buffer,
/// unrecognized generation, malformed record array) rather than erroring. A
/// parse failure here must never cause otherwise CRC-verified, valid
/// downloaded data to be discarded — see `save_download`'s fallback naming.
pub fn extract_vrn(generation: u8, data: &[u8]) -> Option<String> {
    let vrn = match generation {
        1 => extract_vrn_gen1(data),
        2 => extract_vrn_gen2(data),
        other => {
            log::warn!("vu_download: unrecognized generation {} for VRN extraction", other);
            None
        }
    }?;

    if vrn.is_empty() {
        None
    } else {
        Some(vrn)
    }
}

/// `company`'s folder under the configured `download.download_path` (default
/// `~/Documents/tcs/downloads`). Read from the cache on every save, so a change from
/// the UI applies to the next download. Company ids are validated as folder-safe when
/// the config loads (`config::is_valid_company_id`).
fn downloads_dir(company: &str) -> std::io::Result<PathBuf> {
    let configured = get_from_cache(CacheSection::Download, "download_path");
    let root = if configured.trim().is_empty() {
        PathBuf::from(crate::config::default_download_path())
    } else {
        PathBuf::from(configured.trim())
    };
    let dir = root.join(company);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Writes to `<download.download_path>/<company>/<filename>.ddd`. Filename is
/// `<sanitized VRN>_<IMEI>_<YYYYMMDD_HHMMSS>[_<tag>].ddd`, falling back to
/// `UNKNOWN_VRN_<IMEI>_<timestamp>[_<tag>].ddd` when VRN wasn't extractable —
/// a parse failure on our side must never mean data gets thrown away. `tag`
/// is `None` for a normal CRC-verified save, or e.g. `Some("crc_mismatch")`/
/// `Some("incomplete")` for a best-effort debug save of data that didn't pass
/// verification or never finished arriving — clearly marked in the filename
/// so it's never confused with a verified-good download. Note: there is no
/// regulation-mandated filename convention (checked against Annex 1C
/// Appendix 7, requirement DDP_034 — the only rule is one physical file per
/// VU download session), so this naming is our own choice.
pub fn save_download(company: &str, vrn: Option<&str>, imei: &str, data: &[u8], tag: Option<&str>) -> std::io::Result<PathBuf> {
    let name = match vrn {
        Some(v) => sanitize_for_filename(v),
        None => "UNKNOWN_VRN".to_string(),
    };
    save_file(company, &name, imei, data, tag)
}

/// `save_download` for a driver card section (see `card_files`), in its
/// own file beside the VU's: `DRIVER_<card number>_<IMEI>_<timestamp>[_<tag>].ddd`,
/// or `DRIVER_UNKNOWN_...` when the card number wasn't extractable.
pub fn save_card_download(company: &str, card_number: Option<&str>, imei: &str, data: &[u8], tag: Option<&str>) -> std::io::Result<PathBuf> {
    let name = format!("DRIVER_{}", card_number.map(sanitize_for_filename).unwrap_or_else(|| "UNKNOWN".to_string()));
    save_file(company, &name, imei, data, tag)
}

fn save_file(company: &str, name: &str, imei: &str, data: &[u8], tag: Option<&str>) -> std::io::Result<PathBuf> {
    let dir = downloads_dir(company)?;
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let filename = match tag {
        Some(tag) => format!("{}_{}_{}_{}.ddd", name, imei, timestamp, tag),
        None => format!("{}_{}_{}.ddd", name, imei, timestamp),
    };
    let path = dir.join(filename);
    std::fs::write(&path, data)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real bytes captured from an actual Gen2 VU download (Overview block, from
    // the start through the VehicleRegistrationNumberRecordArray) — this is what
    // caught the real bug: an earlier version of this parser didn't account for
    // the 2-byte preamble before the first record array, so it read `76 21 04 00`
    // as the header instead of `04 00 CC 00`, producing a nonsense record size
    // (8452) instead of failing loudly. Byte-math looking correct on paper isn't
    // enough here — this fixture is what actually caught it.
    const GEN2_OVERVIEW_PREFIX_HEX: &str = "76210400cc00017f2181c87f4e81815f2901004208fd45432001ffff015f4c07ff534d5244540e7f494d06082a8648ce3d030107864104605950efddcef23ef98a135b768615c20e2b7449e83d9932f311c6a354641a0488bd16a714e032d061e7917851b8a47b42fc9822224c73ddb21ce05793fda2675f20082c53202001dddd015f25045c1adb805f24047c8c85805f37405b839f8a64a608eacc30524a90d0b89bbb2d254db4fbf5b294bb8b1dbab032ba3320e1eb1bc09b8e0797229e165edc9e2470a3d380fd0d122f319db5b83f50d30f00cc00017f2181c87f4e81815f29010042082c53202001dddd015f4c07ff534d524454137f494d06082a8648ce3d0301078641049728dab06add3bec643efdaecd523a08dc2ea480d88ac57f9fa85b9ccb78144e1df010eb2681a7a3f3875d42b247f022db1e93d52c951191a874fc1750d76a5c5f200800002eb8121806a25f25045c3539805f240478e47c7f5f37400b3ecbd972517fee97412d947fff1dc3658d889325791624051c098e4c1cfb5c792e3ba6ec341ce058aceabc065704bcb2d314382a5279c05c7807ae1f966f3d0a0011000153544f4e455249444745763830783131330b000e0001014c443231535452202020202020";

    #[test]
    fn extract_vrn_gen2_matches_real_captured_download() {
        let data = hex::decode(GEN2_OVERVIEW_PREFIX_HEX).expect("valid hex fixture");
        assert_eq!(extract_vrn(2, &data), Some("LD21STR".to_string()));
    }

    // Real bytes from a Gen2V2 VU download (Overview block, through the
    // VehicleRegistrationNumberRecordArray), whose record has a codePage byte
    // between nation and the registration.
    const GEN2V2_OVERVIEW_PREFIX_HEX: &str = "76310400cd00017f2181c97f4e81825f2901004208fd45432001ffff015f4c07ff534d5244540e7f494e06092b240303020801010786410482a15e1797f162a0974009da1018b43f3f5884a1a9e43180be938ccad741cecca18d6311727603e17d5b5acb55ff4f2fe6ef416ae9fa776516b59b652d37eeb45f20080d4420200d5655015f2504642e0b805f240484a107005f3740a8abff821f20b329f44b05fddbde1aabc12fe32dc1b807146850f3740f28a073a744232a0a9336e9c14defee9db3251aa2ccac135a0c210eaf7793b8bb89ba260f00cd00017f2181c97f4e81825f29010042080d4420200d5655015f4c07ff534d524454137f494e06092b240303020801010786410427b662f9ac97053379ee894b5abccb574289c7447baa4af847dfcfe0d91b24e38d01d3cdcf7c7303c7d22ad566568462c9ff621de9e07516c1f581a3616c35a65f20080274bbb11223ffa15f2504657aaeb35f24048228a0335f3740245d9a30d2cce55770bf0dd7b0f81b24f98fdd9a1ea07e7207f23ccb5cab24f59a9f01ff716031b28f387b0333e5bb99ec7235c7117dff3aac452d2fba404dd00a00110001574442393633303230324c37323932323524000f00011501534a31334b5848202020202020";

    #[test]
    fn extract_vrn_gen2v2_skips_the_code_page() {
        let data = hex::decode(GEN2V2_OVERVIEW_PREFIX_HEX).expect("valid hex fixture");
        assert_eq!(extract_vrn(2, &data), Some("SJ13KXH".to_string()));
        assert_eq!(validate_gen2_structure(&data), Ok(4));
    }

    /// A made-up driver card section: EF_Identification's data (member state, then
    /// the 16-character card number) and its signature.
    fn card_section() -> Vec<u8> {
        let mut card = vec![0x05, 0x20, 0x00, 0x00, 0x11, 0x0d];
        card.extend_from_slice(b"DB12345678901200");
        card.extend_from_slice(&[0x05, 0x20, 0x01, 0x00, 0x02, 0xaa, 0xbb]);
        card
    }

    #[test]
    fn sections_are_kept_apart_and_the_crc_covers_them_in_order() {
        let mut download = DownloadData::default();
        download.add(Section::Vu, &[1, 2]);
        download.add(Section::DriverCard, &[3]);
        download.add(Section::Vu, &[4]);
        assert_eq!((download.vu.as_slice(), download.driver_card.as_slice()), (&[1, 2, 4][..], &[3][..]));
        assert_eq!(download.len(), 4);
        assert_eq!(download.crc32(), crc32fast::hash(&[1, 2, 3, 4]));
    }

    #[test]
    fn truncating_keeps_the_first_bytes_in_the_order_they_arrived() {
        let mut download = DownloadData::default();
        download.add(Section::Vu, &[1, 2]);
        download.add(Section::DriverCard, &[3, 4]);
        download.add(Section::Vu, &[5]);

        download.truncate(3);
        assert_eq!((download.vu.as_slice(), download.driver_card.as_slice()), (&[1, 2][..], &[3][..]));
        assert_eq!(download.crc32(), crc32fast::hash(&[1, 2, 3]));

        // Carrying on after the cut.
        download.add(Section::DriverCard, &[6]);
        assert_eq!(download.crc32(), crc32fast::hash(&[1, 2, 3, 6]));

        download.truncate(10); // more than there is: unchanged
        assert_eq!(download.len(), 4);
        download.truncate(0);
        assert!(download.is_empty());
        assert_eq!(download.crc32(), crc32fast::hash(&[]));
    }

    #[test]
    fn card_files_are_read_with_or_without_the_trep_separator() {
        let mut section = vec![0x76, 0x06];
        section.extend_from_slice(&card_section());
        for section in [&section[..], &card_section()[..]] {
            let card = card_files(section);
            assert_eq!(validate_card_structure(card), Ok(2));
            assert_eq!(extract_card_number(card), Some("DB12345678901200".to_string()));
        }
    }

    #[test]
    fn validate_card_structure_reports_offset_of_a_truncated_file() {
        let card = card_section();
        assert_eq!(validate_card_structure(&card[..card.len() - 1]), Err(22));
    }

    #[test]
    fn control_characters_are_kept_out_of_filenames() {
        assert_eq!(sanitize_for_filename("\u{1}SJ13KXH\t"), "SJ13KXH");
    }

    #[test]
    fn validate_gen2_structure_accepts_the_real_fixture() {
        // The fixture happens to end exactly on a record-array boundary (right
        // after the VRN record), so a clean walk should land exactly on the end
        // and report the 4 record arrays it found.
        let data = hex::decode(GEN2_OVERVIEW_PREFIX_HEX).expect("valid hex fixture");
        assert_eq!(validate_gen2_structure(&data), Ok(4));
    }

    #[test]
    fn validate_gen2_structure_reports_offset_of_a_truncated_block() {
        // Cut off the tail of the VRN record's data (not its header) — the walk
        // should get through the first 3 record arrays cleanly, then fail
        // exactly at the start of the 4th (offset 442) once that one's declared
        // 14 bytes no longer fit in what's left of the buffer.
        let mut data = hex::decode(GEN2_OVERVIEW_PREFIX_HEX).expect("valid hex fixture");
        data.truncate(450); // was 461; VRN record array header starts at 442
        assert_eq!(validate_gen2_structure(&data), Err(442));
    }
}
