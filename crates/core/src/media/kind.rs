//! The kinds of media Rappel keeps, and how a file's kind is told from
//! its first bytes (ADR 0054).

/// The largest image, in bytes (SPEC §4.3; decimal, as GNOME shows sizes).
pub const IMAGE_MAX_BYTES: u64 = 10_000_000;

/// The largest audio file, in bytes (SPEC §4.9).
pub const AUDIO_MAX_BYTES: u64 = 20_000_000;

/// How many of a file's first bytes [`MediaKind::detect`] looks at: an Ogg
/// page header with the longest segment table (27 + 255 bytes) and the
/// codec's signature after it fit.
pub const DETECT_BYTES: usize = 512;

/// A kind of media file. Each has one extension and one MIME type, which
/// are what Rappel writes and the only ones it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MediaKind {
    /// PNG.
    Png,
    /// JPEG (JFIF, Exif or raw).
    Jpeg,
    /// WebP (lossy, lossless or extended).
    WebP,
    /// GIF (87a or 89a), shown still (ADR 0055).
    Gif,
    /// Ogg holding Opus or Vorbis audio (not video, not other codecs).
    Ogg,
    /// MP3 (MPEG audio layer III), with or without an ID3 tag.
    Mp3,
    /// M4A: an ISO base media file whose brand says audio (`M4A `,
    /// `M4B `).
    M4a,
    /// WAV (RIFF/WAVE).
    Wav,
}

impl MediaKind {
    /// Every kind, images first.
    pub const ALL: [Self; 8] = [
        Self::Png,
        Self::Jpeg,
        Self::WebP,
        Self::Gif,
        Self::Ogg,
        Self::Mp3,
        Self::M4a,
        Self::Wav,
    ];

    /// The file name extension, without the dot, in lower case.
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
            Self::Gif => "gif",
            Self::Ogg => "ogg",
            Self::Mp3 => "mp3",
            Self::M4a => "m4a",
            Self::Wav => "wav",
        }
    }

    /// The MIME type, as the database and an export store it.
    #[must_use]
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
            Self::Ogg => "audio/ogg",
            Self::Mp3 => "audio/mpeg",
            Self::M4a => "audio/mp4",
            Self::Wav => "audio/wav",
        }
    }

    /// Whether it is an image (else it is audio).
    #[must_use]
    pub fn is_image(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg | Self::WebP | Self::Gif)
    }

    /// The largest file of this kind, in bytes.
    #[must_use]
    pub fn max_bytes(self) -> u64 {
        if self.is_image() {
            IMAGE_MAX_BYTES
        } else {
            AUDIO_MAX_BYTES
        }
    }

    /// The kind whose extension is exactly `ext` (lower case, no dot).
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.extension() == ext)
    }

    /// The kind whose MIME type is exactly `mime`.
    #[must_use]
    pub fn from_mime(mime: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.mime() == mime)
    }

    /// The kind of a file from its first bytes (at most [`DETECT_BYTES`]
    /// are looked at; pass what there is). Only the signature is checked:
    /// a file that starts like a PNG is a PNG here even if it is cut short
    /// or damaged further on, which the decoders of slices 3 and 4 find.
    /// SVG, HTML, XML, video and anything else is `None`.
    #[must_use]
    pub fn detect(head: &[u8]) -> Option<Self> {
        let head = &head[..head.len().min(DETECT_BYTES)];
        if head.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(Self::Png);
        }
        if head.starts_with(&[0xff, 0xd8, 0xff]) {
            return Some(Self::Jpeg);
        }
        if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
            return Some(Self::Gif);
        }
        if head.len() >= 12 && head.starts_with(b"RIFF") {
            return match &head[8..12] {
                b"WEBP" => Some(Self::WebP),
                b"WAVE" => Some(Self::Wav),
                _ => None,
            };
        }
        if head.starts_with(b"OggS") {
            return ogg_audio(head).then_some(Self::Ogg);
        }
        if m4a(head) {
            return Some(Self::M4a);
        }
        if mp3(head) {
            return Some(Self::Mp3);
        }
        None
    }
}

/// An Ogg stream whose first page starts an Opus or Vorbis stream: version
/// 0, the "beginning of stream" flag, and the codec's identification
/// header as the first packet, right after the segment table.
fn ogg_audio(head: &[u8]) -> bool {
    let (Some(&version), Some(&flags), Some(&segments)) = (head.get(4), head.get(5), head.get(26))
    else {
        return false;
    };
    if version != 0 || flags & 0x02 == 0 {
        return false;
    }
    let Some(packet) = head.get(27 + usize::from(segments)..) else {
        return false;
    };
    packet.starts_with(b"OpusHead") || packet.starts_with(b"\x01vorbis")
}

/// An ISO base media file (`ftyp` first) whose major brand or one of whose
/// compatible brands is an audio-only one. A video MP4 (`isom`, `mp42`
/// without them) is not audio.
fn m4a(head: &[u8]) -> bool {
    if head.len() < 16 || &head[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let end = usize::try_from(size).unwrap_or(usize::MAX).min(head.len());
    let audio = |brand: &[u8]| brand == b"M4A " || brand == b"M4B ";
    // Major brand at 8, minor version at 12, compatible brands from 16.
    audio(&head[8..12])
        || head
            .get(16..end)
            .is_some_and(|brands| brands.as_chunks::<4>().0.iter().any(|b| audio(b)))
}

/// MP3: an ID3 tag (`ID3`, major version 2 to 4), or an MPEG audio frame
/// header of layer III with a valid version, bit rate and sample rate.
fn mp3(head: &[u8]) -> bool {
    if head.len() >= 4 && head.starts_with(b"ID3") {
        return (2..=4).contains(&head[3]);
    }
    let [a, b, c, ..] = *head else {
        return false;
    };
    let sync = a == 0xff && b & 0xe0 == 0xe0;
    let version = (b >> 3) & 0b11;
    let layer = (b >> 1) & 0b11;
    let bitrate = c >> 4;
    let rate = (c >> 2) & 0b11;
    sync && version != 0b01 && layer == 0b01 && bitrate != 0b1111 && rate != 0b11
}
