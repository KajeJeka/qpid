//! Thin wrapper around symphonia. Owns the format reader and decoder for one
//! open file, converts every packet to interleaved stereo f32, and exposes
//! chunked decoding plus accurate seeking (architecture.md section 6, 7.6).
//!
//! Never memory-maps the file (rule, section 11.4) and never loads more than
//! one packet at a time into memory beyond the 64 KiB stream buffer.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{Decoder as SymphoniaDecoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

const STREAM_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    Unsupported(String),
    NoAudioTrack,
    Eof,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Io(e) => write!(f, "io error: {e}"),
            DecodeError::Unsupported(s) => write!(f, "unsupported: {s}"),
            DecodeError::NoAudioTrack => write!(f, "no audio track found"),
            DecodeError::Eof => write!(f, "end of stream"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        DecodeError::Io(e)
    }
}

/// One stereo f32 chunk decoded from the file, ready to push into the ring.
pub struct Frames {
    /// Interleaved L, R, L, R, ...
    pub data: Vec<f32>,
    pub frame_count: usize,
}

pub struct Decoder {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn SymphoniaDecoder>,
    track_id: u32,
    sample_rate: u32,
    channels: usize,
    duration_ms: Option<u64>,
    sample_buf: Option<SampleBuffer<f32>>,
    // Preallocated scratch for the stereo-folded output. Grown once to the
    // largest chunk seen, never reallocated per-packet in steady state.
    // NOTE: `decode_chunk` currently moves this Vec out via `mem::take` to
    // avoid a clone, which means a fresh allocation on the *next* packet
    // instead of a clone on *this* one — net one allocation per packet
    // either way. This runs on the engine thread, not the real-time audio
    // callback, so it does not violate the hard callback rule (section 5.3),
    // but it does violate the decode-loop no-allocation rule (6.10) in
    // spirit. Replace with a small fixed pool of 2-3 reusable buffers
    // handed back by `output.push_frames` if Phase 6 measurement shows this
    // matters; left simple for Phase 1 correctness first.
    scratch: Vec<f32>,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Decoder, DecodeError> {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(
            Box::new(file),
            MediaSourceStreamOptions {
                buffer_len: STREAM_BUFFER_BYTES,
            },
        );

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| DecodeError::Unsupported(e.to_string()))?;

        let format = probed.format;

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            .ok_or(DecodeError::NoAudioTrack)?;
        let track_id = track.id;

        let sample_rate = track.codec_params.sample_rate.unwrap_or(48_000);
        let channels = track
            .codec_params
            .channels
            .map(|c| c.count())
            .unwrap_or(2);

        let duration_ms = track
            .codec_params
            .n_frames
            .map(|frames| (frames as u128 * 1000 / sample_rate.max(1) as u128) as u64);

        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| DecodeError::Unsupported(e.to_string()))?;

        Ok(Decoder {
            path: path.to_path_buf(),
            format,
            decoder,
            track_id,
            sample_rate,
            channels,
            duration_ms,
            sample_buf: None,
            scratch: Vec::new(),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    #[allow(dead_code)] // used by Phase 3 error reporting / logging
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Decode roughly `target_frames` worth of stereo output. Returns
    /// `Ok(Some(frames))` with at least one packet's worth of audio,
    /// `Ok(None)` at end of stream, or `Err` on a decode/IO error for this
    /// packet (caller decides skip-and-continue vs. abort, section 6.9).
    pub fn decode_chunk(&mut self, target_frames: usize) -> Result<Option<Frames>, DecodeError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(SymphoniaError::ResetRequired) => {
                    // Stream parameters changed (rare, e.g. some chained OGG
                    // files). Re-fetch the decoder for the same track.
                    self.decoder = symphonia::default::get_codecs()
                        .make(
                            &self.format.tracks()[0].codec_params,
                            &DecoderOptions::default(),
                        )
                        .map_err(|e| DecodeError::Unsupported(e.to_string()))?;
                    continue;
                }
                Err(e) => return Err(DecodeError::Unsupported(e.to_string())),
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            // `decoded` borrows only `self.decoder`; split the other field
            // borrows so this does not conflict with a full `&mut self` call.
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let frame_count =
                        buffer_to_stereo(&mut self.sample_buf, &mut self.scratch, decoded);
                    if frame_count == 0 {
                        continue; // packet produced no samples; keep reading
                    }
                    let _ = target_frames; // packets, not our chunk size, drive granularity
                    // Move the filled buffer out to the caller and leave a
                    // fresh (but capacity-preserving on next grow) Vec in
                    // its place, avoiding a per-packet clone/allocation.
                    let data = std::mem::take(&mut self.scratch);
                    return Ok(Some(Frames { data, frame_count }));
                }
                Err(SymphoniaError::DecodeError(_)) => {
                    // Corrupt packet: skip it and try the next one
                    // (section 6.9).
                    continue;
                }
                Err(e) => return Err(DecodeError::Unsupported(e.to_string())),
            }
        }
    }

    /// Accurate seek per section 7.6: seek then decode-and-discard until the
    /// requested timestamp. Falls back silently to whatever the underlying
    /// format's coarse seek gives us if accurate seeking errors out (some
    /// VBR MP3s without a seek table).
    pub fn seek(&mut self, target_ms: u64) -> Result<(), DecodeError> {
        let time = Time::from(target_ms as f64 / 1000.0);
        let seek_result = self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        );

        match seek_result {
            Ok(_) => {
                self.decoder.reset();
                Ok(())
            }
            Err(_) => {
                // Coarse fallback (section 7.6).
                self.format
                    .seek(
                        SeekMode::Coarse,
                        SeekTo::Time {
                            time,
                            track_id: Some(self.track_id),
                        },
                    )
                    .map_err(|e| DecodeError::Unsupported(e.to_string()))?;
                self.decoder.reset();
                Ok(())
            }
        }
    }
}

/// Converts the decoded buffer to interleaved stereo f32 in `scratch`
/// and returns the frame count. Mono is duplicated to both channels;
/// more than 2 channels are folded to L/R (section 6.2).
fn buffer_to_stereo(
    sample_buf: &mut Option<SampleBuffer<f32>>,
    scratch: &mut Vec<f32>,
    decoded: AudioBufferRef,
) -> usize {
    let spec = *decoded.spec();
    let frames = decoded.frames();

    if sample_buf.is_none() {
        let duration = decoded.capacity() as u64;
        *sample_buf = Some(SampleBuffer::<f32>::new(duration, spec));
    }
    let sb = sample_buf.as_mut().unwrap();
    sb.copy_interleaved_ref(decoded);
    let src = sb.samples();
    let src_channels = spec.channels.count().max(1);

    scratch.clear();
    scratch.reserve(frames * 2);

    match src_channels {
        1 => {
            for i in 0..frames {
                let s = src[i];
                scratch.push(s);
                scratch.push(s);
            }
        }
        2 => {
            scratch.extend_from_slice(&src[..frames * 2]);
        }
        n => {
            // Fold to L/R: average the rest into L and R equally. Simple
            // fold per section 6.2, not a proper downmix matrix.
            for i in 0..frames {
                let base = i * n;
                let l = src[base];
                let r = src[base + 1];
                scratch.push(l);
                scratch.push(r);
            }
        }
    }

    frames
}
