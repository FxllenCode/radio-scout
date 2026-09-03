//! A ZIP file as a sequence of values (#65, spec US 33).
//!
//! Hand-written rather than a dependency, for [`crate::enhance`]'s reason one
//! format along: this is the whole of what we need of the format — one entry
//! per Call, stored uncompressed because a WAV is the only compressible thing
//! in an archive full of MP3 and the Pi doing the compressing is the one that
//! can least afford it — and because the `zip` crate wants a synchronous
//! `Write + Seek` sink, which an export streamed out of an object store over an
//! `axum` body is not. Bridging one to the other would mean a `spawn_blocking`
//! and a channel in order to reach code that would then do exactly this.
//!
//! # Why the sizes come *after* the bytes
//!
//! A local file header states a CRC and two lengths, and an export learns all
//! three only by reading the object — which is precisely what it must not do
//! twice, and must not hold whole. So every entry sets **general-purpose bit
//! 3** and writes those three numbers into a **data descriptor** behind the
//! data instead. That is the format's own answer to streaming, it is what every
//! `zip` implementation since 2.0 reads, and it is what makes an export's
//! memory one Call rather than one range.
//!
//! # Why a caller cannot forget to close an entry
//!
//! [`ZipStream::begin`] hands back an [`Open`], and an [`Open`] is the only
//! thing that can produce the entry's central-directory record — it does so in
//! [`Open::end`], which consumes it. An entry begun and not ended is therefore
//! a value left lying around rather than an archive whose index is quietly
//! missing a file, which is the shape this kind of writer usually gets wrong.

use bytes::Bytes;

/// The local file header, before the name.
const LOCAL_HEADER_LEN: u64 = 30;
/// Signature, CRC and the two lengths.
const DATA_DESCRIPTOR_LEN: u64 = 16;

/// `Store`: the bytes go in as they are. See the module note — an archive of
/// Calls is already-compressed audio, and deflating a WAV on a Pi buys a few
/// per cent for the CPU the recorder is using.
const STORED: u16 = 0;
/// Bit 3 (sizes follow the data) and bit 11 (the name is UTF-8).
const STREAMED_UTF8: u16 = 0x0008 | 0x0800;
/// What a reader needs to understand: 2.0, which is where `Store` plus a data
/// descriptor has been since 1993.
const VERSION: u16 = 20;

/// A ZIP archive being written, as a value: bytes in, bytes out, no sink.
#[derive(Debug, Default)]
pub struct ZipStream {
    /// How many bytes have been handed to the caller so far — the offset the
    /// next local header lands at, which the central directory has to state.
    offset: u64,
    entries: Vec<Central>,
}

/// One entry's row in the central directory, filled in as it is written.
#[derive(Debug)]
struct Central {
    name: String,
    at_ms: i64,
    offset: u64,
    crc: u32,
    size: u64,
}

/// An entry whose data is being written. Consumed by [`Open::end`], which is
/// the only place a [`Central`] comes from.
#[derive(Debug)]
pub struct Open {
    name: String,
    at_ms: i64,
    offset: u64,
    crc: Crc32,
    size: u64,
}

impl ZipStream {
    pub fn new() -> Self {
        ZipStream::default()
    }

    /// Start an entry, and hand back the local header to send.
    pub fn begin(&mut self, name: &str, at_ms: i64) -> (Open, Bytes) {
        let offset = self.offset;
        let name_bytes = name.as_bytes();
        let (date, time) = dos_stamp(at_ms);

        let mut out = Vec::with_capacity(LOCAL_HEADER_LEN as usize + name_bytes.len());
        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend(VERSION.to_le_bytes());
        out.extend(STREAMED_UTF8.to_le_bytes());
        out.extend(STORED.to_le_bytes());
        out.extend(time.to_le_bytes());
        out.extend(date.to_le_bytes());
        // CRC and both lengths ride in the data descriptor instead.
        out.extend([0u8; 12]);
        out.extend((name_bytes.len() as u16).to_le_bytes());
        out.extend(0u16.to_le_bytes()); // no extra field
        out.extend(name_bytes);

        self.offset += out.len() as u64;
        (
            Open {
                name: name.to_string(),
                at_ms,
                offset,
                crc: Crc32::new(),
                size: 0,
            },
            Bytes::from(out),
        )
    }

    /// Close the archive: every entry's central-directory record, then the
    /// end-of-central-directory that says where they start.
    pub fn finish(self) -> Bytes {
        let start = self.offset;
        let mut out = Vec::new();
        for entry in &self.entries {
            let name = entry.name.as_bytes();
            let (date, time) = dos_stamp(entry.at_ms);
            out.extend(0x0201_4b50u32.to_le_bytes());
            out.extend(VERSION.to_le_bytes()); // version made by
            out.extend(VERSION.to_le_bytes()); // version needed
            out.extend(STREAMED_UTF8.to_le_bytes());
            out.extend(STORED.to_le_bytes());
            out.extend(time.to_le_bytes());
            out.extend(date.to_le_bytes());
            out.extend(entry.crc.to_le_bytes());
            out.extend((entry.size as u32).to_le_bytes()); // compressed
            out.extend((entry.size as u32).to_le_bytes()); // uncompressed
            out.extend((name.len() as u16).to_le_bytes());
            out.extend(0u16.to_le_bytes()); // extra
            out.extend(0u16.to_le_bytes()); // comment
            out.extend(0u16.to_le_bytes()); // disk
            out.extend(0u16.to_le_bytes()); // internal attributes
            out.extend(0u32.to_le_bytes()); // external attributes
            out.extend((entry.offset as u32).to_le_bytes());
            out.extend(name);
        }
        let directory_len = out.len() as u32;

        let count = self.entries.len() as u16;
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend(0u16.to_le_bytes()); // this disk
        out.extend(0u16.to_le_bytes()); // disk the directory starts on
        out.extend(count.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend(directory_len.to_le_bytes());
        out.extend((start as u32).to_le_bytes());
        out.extend(0u16.to_le_bytes()); // no archive comment
        Bytes::from(out)
    }
}

impl Open {
    /// Take the next piece of this entry's data on its way past: the CRC and
    /// the length are accumulated here, and the caller is handed back exactly
    /// what it gave. Returning the bytes is what makes "record it" and "send
    /// it" one act instead of two a caller can do only one of.
    pub fn chunk(&mut self, bytes: Bytes) -> Bytes {
        self.crc.update(&bytes);
        self.size += bytes.len() as u64;
        bytes
    }

    /// End the entry: the data descriptor to send, and the archive's index
    /// gains the row only this can produce.
    pub fn end(self, zip: &mut ZipStream) -> Bytes {
        let crc = self.crc.finish();
        let mut out = Vec::with_capacity(DATA_DESCRIPTOR_LEN as usize);
        out.extend(0x0807_4b50u32.to_le_bytes());
        out.extend(crc.to_le_bytes());
        out.extend((self.size as u32).to_le_bytes()); // compressed
        out.extend((self.size as u32).to_le_bytes()); // uncompressed

        zip.offset += self.size + DATA_DESCRIPTOR_LEN;
        zip.entries.push(Central {
            name: self.name,
            at_ms: self.at_ms,
            offset: self.offset,
            crc,
            size: self.size,
        });
        Bytes::from(out)
    }
}

/// The MS-DOS date and time an entry is stamped with, from unix milliseconds.
///
/// UTC, deliberately: the Instance's timezone is not the recipient's, and a ZIP
/// timestamp carries no zone to say which was meant. Clamped into the range the
/// format can express (1980–2107), because a Call stamped by a recorder whose
/// clock had not yet synced is a real thing and an archive that cannot be
/// opened because of one is not.
fn dos_stamp(at_ms: i64) -> (u16, u16) {
    let seconds = at_ms.div_euclid(1000);
    let at = time::OffsetDateTime::from_unix_timestamp(seconds)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let year = at.year().clamp(1980, 2107);
    let date = (((year - 1980) as u16) << 9) | ((at.month() as u16) << 5) | at.day() as u16;
    let time = ((at.hour() as u16) << 11) | ((at.minute() as u16) << 5) | (at.second() as u16 / 2);
    (date, time)
}

/// CRC-32/ISO-HDLC, which is the only checksum a ZIP entry may carry.
#[derive(Debug, Clone, Copy)]
struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Crc32(0xFFFF_FFFF)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            let index = ((self.0 ^ *byte as u32) & 0xFF) as usize;
            self.0 = (self.0 >> 8) ^ CRC_TABLE[index];
        }
    }

    fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

/// The reversed-polynomial table, built once at compile time rather than
/// written out: 256 hex constants are 256 chances to mistype one, and a wrong
/// entry would corrupt only the archives whose bytes happened to reach it.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    /// Read a little-endian `u32` at `offset`.
    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
    }

    fn u16_at(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("two bytes"))
    }

    /// One archive, built the way the export builds one.
    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = ZipStream::new();
        let mut out = Vec::new();
        for (name, data) in entries {
            let (mut open, header) = zip.begin(name, 1_700_000_000_000);
            out.extend_from_slice(&header);
            out.extend_from_slice(&open.chunk(Bytes::copy_from_slice(data)));
            out.extend_from_slice(&open.end(&mut zip));
        }
        out.extend_from_slice(&zip.finish());
        out
    }

    /// The check value every CRC-32/ISO-HDLC implementation is published with.
    /// Hand-rolling the table is only safe because this pins it.
    #[test]
    fn the_checksum_is_the_one_zip_readers_expect() {
        let mut crc = Crc32::new();
        crc.update(b"123456789");

        assert_eq!(crc.finish(), 0xCBF4_3926);
    }

    /// The whole reason for the data descriptor: the header claims nothing
    /// about the data, and the three numbers arrive behind it.
    #[test]
    fn an_entry_states_its_crc_and_length_behind_its_data() {
        let data = b"the county at two in the morning";

        let bytes = archive(&[("call.wav", data)]);

        assert_eq!(u32_at(&bytes, 0), 0x0403_4b50, "local file header");
        assert_eq!(u16_at(&bytes, 6) & 0x0008, 0x0008, "bit 3: sizes follow");
        assert_eq!(u32_at(&bytes, 14), 0, "the header claims no CRC");
        assert_eq!(u32_at(&bytes, 18), 0, "...and no compressed size");
        assert_eq!(u32_at(&bytes, 22), 0, "...and no uncompressed size");

        let descriptor = 30 + "call.wav".len() + data.len();
        assert_eq!(u32_at(&bytes, descriptor), 0x0807_4b50, "data descriptor");
        let mut expected = Crc32::new();
        expected.update(data);
        assert_eq!(u32_at(&bytes, descriptor + 4), expected.finish());
        assert_eq!(u32_at(&bytes, descriptor + 8), data.len() as u32);
        assert_eq!(u32_at(&bytes, descriptor + 12), data.len() as u32);
    }

    /// An archive is read by seeking to the offsets its index states, so an
    /// index that is off by even the length of a data descriptor is an archive
    /// no reader can open. Asserted by following each offset and finding a
    /// local header with the right name under it.
    #[test]
    fn the_index_points_at_each_entrys_own_header() {
        let bytes = archive(&[("manifest.json", b"{}"), ("a.wav", b"RIFF"), ("b.wav", b"")]);

        // The end-of-central-directory is the last 22 bytes (no comment).
        let eocd = bytes.len() - 22;
        assert_eq!(
            u32_at(&bytes, eocd),
            0x0605_4b50,
            "end of central directory"
        );
        assert_eq!(u16_at(&bytes, eocd + 10), 3, "three entries");
        let directory = u32_at(&bytes, eocd + 16) as usize;
        assert_eq!(
            u32_at(&bytes, eocd + 12) as usize,
            eocd - directory,
            "the directory's stated length reaches exactly to the end record"
        );

        let mut at = directory;
        for name in ["manifest.json", "a.wav", "b.wav"] {
            assert_eq!(u32_at(&bytes, at), 0x0201_4b50, "central directory header");
            let name_len = u16_at(&bytes, at + 28) as usize;
            assert_eq!(&bytes[at + 46..at + 46 + name_len], name.as_bytes());

            let local = u32_at(&bytes, at + 42) as usize;
            assert_eq!(u32_at(&bytes, local), 0x0403_4b50, "{name}'s own header");
            assert_eq!(
                &bytes[local + 30..local + 30 + name_len],
                name.as_bytes(),
                "the index sent us to the wrong entry"
            );
            at += 46 + name_len;
        }
        assert_eq!(at, eocd, "the directory ends where the end record begins");
    }

    /// The index also has to agree with the data descriptors, or an extractor
    /// that trusts one over the other writes a truncated file.
    #[test]
    fn the_index_repeats_what_each_descriptor_said() {
        let data = b"a longer transmission, several seconds of it".as_slice();
        let bytes = archive(&[("a.wav", data)]);

        let eocd = bytes.len() - 22;
        let directory = u32_at(&bytes, eocd + 16) as usize;
        let descriptor = 30 + "a.wav".len() + data.len();

        assert_eq!(
            u32_at(&bytes, directory + 16),
            u32_at(&bytes, descriptor + 4)
        );
        assert_eq!(u32_at(&bytes, directory + 20), data.len() as u32);
        assert_eq!(u32_at(&bytes, directory + 24), data.len() as u32);
    }

    /// A ZIP stamp has no timezone and eight bits of year, so both edges are
    /// decisions rather than accidents: UTC, and clamped rather than wrapped —
    /// a recorder whose clock had not synced must not produce an archive that
    /// will not open.
    #[rstest]
    #[case::the_epoch(0, 1980, 1, 1)]
    #[case::a_call_last_year(1_700_000_000_000, 2023, 11, 14)]
    #[case::before_the_format_existed(-1_000_000_000_000, 1980, 4, 24)]
    fn a_stamp_outside_the_format_is_clamped_rather_than_wrapped(
        #[case] at_ms: i64,
        #[case] year: i32,
        #[case] month: u16,
        #[case] day: u16,
    ) {
        let (date, _) = dos_stamp(at_ms);

        assert_eq!(date >> 9, (year - 1980) as u16, "year");
        assert_eq!((date >> 5) & 0xF, month, "month");
        assert_eq!(date & 0x1F, day, "day");
    }

    /// The time of day, to the two-second resolution the format has.
    #[test]
    fn a_stamp_keeps_the_time_of_day_in_utc() {
        // 2023-11-14T22:13:20Z
        let (_, at) = dos_stamp(1_700_000_000_000);

        assert_eq!(at >> 11, 22, "hour");
        assert_eq!((at >> 5) & 0x3F, 13, "minute");
        assert_eq!((at & 0x1F) * 2, 20, "second");
    }

    proptest! {
        /// **How the data was split up must not reach the archive.** An export
        /// hands the stream whatever the object store gave it, in whatever
        /// pieces the manifest happened to be built in — so a CRC or a length
        /// that depended on the chunking would produce archives that differ run
        /// to run and, worse, would be wrong for exactly one of them.
        #[test]
        fn chunking_is_invisible(data in proptest::collection::vec(any::<u8>(), 0..2000), at in 1u8..40) {
            let mut whole = ZipStream::new();
            let (mut open, header) = whole.begin("x.wav", 1_700_000_000_000);
            let mut one = Vec::from(&header[..]);
            one.extend_from_slice(&open.chunk(Bytes::copy_from_slice(&data)));
            one.extend_from_slice(&open.end(&mut whole));
            one.extend_from_slice(&whole.finish());

            let mut split = ZipStream::new();
            let (mut open, header) = split.begin("x.wav", 1_700_000_000_000);
            let mut many = Vec::from(&header[..]);
            for piece in data.chunks(at as usize).collect::<Vec<_>>() {
                many.extend_from_slice(&open.chunk(Bytes::copy_from_slice(piece)));
            }
            many.extend_from_slice(&open.end(&mut split));
            many.extend_from_slice(&split.finish());

            prop_assert_eq!(one, many);
        }
    }
}
