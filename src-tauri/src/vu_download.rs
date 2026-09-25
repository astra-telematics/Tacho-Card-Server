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

/// `hex(SHA256(...))` this is not — just a plain CRC32 over the reassembled
/// file, matching whatever the device computed, so a corrupted/incomplete
/// transfer can be detected and the device told to redownload rather than
/// silently persisting bad data.
pub fn compute_crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

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
    Some(decode_vrn_bytes(vrn_bytes))
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

/// Overview block field order: MemberStateCertificateRecordArray,
/// VuCertificateRecordArray, VehicleIdentificationNumberRecordArray,
/// VehicleRegistrationNumberRecordArray (the one we want) — walk/skip the
/// first three generically using their own declared sizes, then read the
/// fourth using its own declared record size.
///
/// This generation's VehicleRegistrationNumber record is `nation(1) +
/// vehicleRegNumber(13)` — 14 bytes, with no separate codePage byte (unlike
/// the Gen1-era data dictionary's version of this type) — confirmed against
/// the same real capture.
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
    if header.no_of_records == 0 || header.record_size < 1 {
        return None;
    }
    let record = data.get(record_start..record_start + header.record_size as usize)?;
    let vrn_bytes = record.get(1..)?; // skip nation(1)
    Some(decode_vrn_bytes(vrn_bytes))
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
/// trailing bytes too short to be another header. Confirmed against a real
/// 4-block capture (Overview + EventsAndFaults + DetailedSpeed +
/// TechnicalData) that this correctly walks through the first three blocks;
/// TechnicalData's own internal record layout (specifically calibration
/// records, which appear to embed variable-length text fields like a
/// workshop name) isn't understood/verified yet, so a download that includes
/// it will currently — correctly, not as a bug — report a malformed offset
/// once the walk reaches content it doesn't recognize, rather than silently
/// accept it.
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

/// `vehicleRegNumber` is code-page-encoded text. Decoding as ISO-8859-1 (byte
/// value == Unicode code point) for any codePage value is a known
/// simplification — covers the common case without implementing every code
/// page table from the data dictionary. Trims trailing NUL/space padding.
fn decode_vrn_bytes(bytes: &[u8]) -> String {
    let text: String = bytes.iter().map(|&b| b as char).collect();
    text.trim_end_matches(['\0', ' ']).to_string()
}

/// Strips characters that are invalid in filenames on common filesystems
/// (notably Windows, which this project targets first). VRNs are arbitrary
/// per-country strings, so this is defensive rather than expected to trigger
/// often.
fn sanitize_for_filename(s: &str) -> String {
    s.chars().filter(|c| !"/\\:*?\"<>|".contains(*c)).collect()
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

/// The configured `server.download_path`, or the default (`~/Documents/tcs/downloads`)
/// if none is set. Read from the cache on every save, so a change from the UI applies
/// to the next download.
fn downloads_dir() -> std::io::Result<PathBuf> {
    let configured = get_from_cache(CacheSection::Server, "download_path");
    let dir = if configured.trim().is_empty() {
        PathBuf::from(crate::config::default_download_path())
    } else {
        PathBuf::from(configured.trim())
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Writes to `<server.download_path>/<filename>.ddd`. Filename is
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
pub fn save_download(vrn: Option<&str>, imei: &str, data: &[u8], tag: Option<&str>) -> std::io::Result<PathBuf> {
    let dir = downloads_dir()?;
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let vrn_part = match vrn {
        Some(v) => sanitize_for_filename(v),
        None => "UNKNOWN_VRN".to_string(),
    };
    let filename = match tag {
        Some(tag) => format!("{}_{}_{}_{}.ddd", vrn_part, imei, timestamp, tag),
        None => format!("{}_{}_{}.ddd", vrn_part, imei, timestamp),
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
