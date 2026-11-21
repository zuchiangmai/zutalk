//! An audio file: the same blocks as a live recording, as fast as the file decodes. There is
//! no wall clock here; a block's position is the count of samples.

use super::{Msg, Notice, Resampler, TICK};
use crate::clock::{BLOCK, Block, Clock, Gap};
use crate::supervise::End;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;
use symphonia::core::codecs::audio::AudioDecoder;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;

pub(super) struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    samples: Vec<f32>,
    tally: (u32, u32), // packets that decoded, and packets that did not
}

impl Decoder {
    /// Everything that can be known before decoding is checked here, so that a file that
    /// cannot be imported is refused at once, with the reason.
    pub(super) fn open(path: &Path) -> Result<Self, End> {
        let file = std::fs::File::open(path).map_err(|_| End::Fatal("audio.file.open"))?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
            hint.with_extension(extension);
        }
        let (probe, codecs) = (symphonia::default::get_probe(), symphonia::default::get_codecs());
        let format = probe.probe(&hint, stream, Default::default(), Default::default());
        let format = format.map_err(|_| End::Fatal("audio.file.format"))?;
        let track = format.default_track(TrackType::Audio);
        let codec = track.and_then(|track| track.codec_params.as_ref()?.audio());
        let (Some(track), Some(codec)) = (track, codec) else {
            return Err(End::Fatal("audio.file.no_audio"));
        };
        // The decoder refuses what it cannot do, such as a file that declares HE-AAC.
        let decoder = codecs.make_audio_decoder(codec, &Default::default());
        let decoder = decoder.map_err(|_| End::Fatal("audio.file.codec"))?;
        Ok(Self { track: track.id, format, decoder, samples: Vec::new(), tally: (0, 0) })
    }

    /// The next stretch of audio, mixed down to `mono`, and its sample rate. `None` at the
    /// end of the file.
    fn next(&mut self, mono: &mut Vec<f32>) -> Result<Option<u32>, &'static str> {
        loop {
            let Some(packet) = self.format.next_packet().map_err(|_| "audio.file.broken")? else {
                return Ok(None);
            };
            if packet.track_id != self.track {
                continue;
            }
            let audio = match self.decoder.decode(&packet) {
                Ok(audio) => audio,
                // A damaged packet here and there is a click. A file that mostly fails must
                // not be imported as a short, silent recording.
                Err(Error::DecodeError(_)) if self.tally.1 < self.tally.0 + 10 => {
                    self.tally.1 += 1;
                    continue;
                }
                Err(_) => return Err("audio.file.broken"),
            };
            self.tally.0 += 1;
            // The channel count is the decoder's, for this very buffer. The container's idea
            // of it once made stereo AAC come out as mono at half speed.
            let channels = audio.spec().channels().count().max(1);
            audio.copy_to_vec_interleaved(&mut self.samples);
            let frames = self.samples.chunks_exact(channels);
            mono.clear();
            mono.extend(frames.map(|frame| frame.iter().sum::<f32>() / channels as f32));
            return Ok(Some(audio.spec().rate()));
        }
    }
}

/// The file's reader. Paused, it waits; stopped, it leaves the rest of the file.
pub(super) fn run(
    mut decoder: Decoder,
    said: &Receiver<Msg>,
    mut sink: impl FnMut(Block),
    mut notice: impl FnMut(Notice),
) {
    let mut clock = Clock::new(|_| Gap::Keep);
    let (mut mono, mut pcm, mut blocks, mut paused) = (Vec::new(), Vec::new(), Vec::new(), false);
    let mut resampler: Option<(u32, Resampler)> = None;
    loop {
        match said.recv_timeout(if paused { TICK } else { Duration::ZERO }) {
            Ok(Msg::Pause(now, _)) => paused = now,
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            _ => {}
        }
        if paused {
            continue;
        }
        pcm.clear();
        let last = match decoder.next(&mut mono) {
            Ok(Some(rate)) => {
                if resampler.as_ref().is_none_or(|(at, _)| *at != rate) {
                    resampler = Resampler::new(rate).map(|resampler| (rate, resampler));
                }
                let Some((_, resampler)) = &mut resampler else {
                    return notice(Notice::Fatal("audio.file.format"));
                };
                resampler.push(&mono, &mut pcm);
                None
            }
            Ok(None) => {
                // The last block is filled up with silence rather than dropped.
                pcm.resize(BLOCK - 1, 0);
                Some(Notice::Ended)
            }
            Err(why) => Some(Notice::Fatal(why)),
        };
        clock.push(&pcm, 0, &mut blocks);
        blocks.drain(..).for_each(&mut sink);
        if let Some(last) = last {
            return notice(last);
        }
    }
}
