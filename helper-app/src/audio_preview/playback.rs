use crate::bridge::AudioPreviewSource;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};

pub enum Command {
    Load(AudioPreviewSource),
    Toggle,
    Seek(f32),
    Stop,
}

pub struct Snapshot {
    pub position: Duration,
    pub duration: Option<Duration>,
    pub playing: bool,
}

pub struct Update {
    pub generation: u64,
    pub command_completed: bool,
    pub result: Result<Snapshot, &'static str>,
}

pub struct Worker {
    commands: mpsc::Sender<(u64, Command)>,
    updates: mpsc::Receiver<Update>,
    generation: Arc<AtomicU64>,
}

impl Worker {
    pub fn spawn(generation: u64) -> std::io::Result<Self> {
        let (commands, pending) = mpsc::channel();
        let (report, updates) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(generation));
        let latest = generation.clone();
        thread::Builder::new()
            .name("audio-preview".into())
            .spawn(move || run(pending, report, latest))?;
        Ok(Self {
            commands,
            updates,
            generation,
        })
    }

    pub fn send(&self, generation: u64, command: Command) -> bool {
        self.commands.send((generation, command)).is_ok()
    }

    pub fn next_update(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    pub fn cancel(&self, generation: u64) {
        self.generation.store(generation, Ordering::Release);
        self.send(generation, Command::Stop);
    }
}

#[cfg(windows)]
fn run(
    pending: mpsc::Receiver<(u64, Command)>,
    updates: mpsc::Sender<Update>,
    latest: Arc<AtomicU64>,
) {
    use mpsc::RecvTimeoutError;
    use windows::Playback;

    let mut active: Option<Playback> = None;
    let mut generation = latest.load(Ordering::Acquire);
    loop {
        let command = if active.as_ref().is_some_and(Playback::playing) {
            pending.recv_timeout(Duration::from_millis(75))
        } else {
            pending.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        let (command_completed, result) = match command {
            Ok((id, command)) => {
                if id != latest.load(Ordering::Acquire) {
                    continue;
                }
                if id != generation {
                    active = None;
                    generation = id;
                }
                let current = || id == latest.load(Ordering::Acquire);
                let result = match command {
                    Command::Stop => {
                        active = None;
                        continue;
                    }
                    Command::Load(source) => {
                        active = None;
                        Playback::open(source, current).map(|playback| active = Some(playback))
                    }
                    Command::Toggle => active
                        .as_mut()
                        .ok_or("Audio preview is no longer available.")
                        .and_then(|playback| playback.toggle(current)),
                    Command::Seek(fraction) => active
                        .as_mut()
                        .ok_or("Audio preview is no longer available.")
                        .and_then(|playback| playback.seek(fraction, current)),
                };
                (true, result)
            }
            Err(RecvTimeoutError::Timeout) => (false, Ok(())),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if generation != latest.load(Ordering::Acquire) {
            active = None;
            continue;
        }
        let result = result.and_then(|()| {
            active
                .as_mut()
                .ok_or("Audio preview is no longer available.")?
                .snapshot()
        });
        if result.is_err() {
            active = None;
        }
        if updates
            .send(Update {
                generation,
                command_completed,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

#[cfg(not(windows))]
fn run(
    pending: mpsc::Receiver<(u64, Command)>,
    updates: mpsc::Sender<Update>,
    latest: Arc<AtomicU64>,
) {
    for (generation, command) in pending {
        if generation != latest.load(Ordering::Acquire) {
            continue;
        }
        if let Command::Load(source) = command {
            drop(source);
            if updates
                .send(Update {
                    generation,
                    command_completed: true,
                    result: Err("Audio preview requires Windows."),
                })
                .is_err()
            {
                break;
            }
        } else if let Command::Seek(fraction) = command {
            let _ = fraction;
        }
    }
}

#[cfg(any(windows, test))]
mod windows {
    use super::*;
    use rodio::{Decoder, Source};
    use std::{fs::File, io::BufReader, path::Path};

    type FileDecoder = Decoder<BufReader<File>>;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Range {
        start: Duration,
        length: Option<Duration>,
    }

    impl Range {
        fn from_marks(source: &AudioPreviewSource, file_duration: Option<Duration>) -> Self {
            let mark = |seconds: f64| Duration::try_from_secs_f64(seconds).ok();
            let start = source.in_seconds.and_then(mark).unwrap_or_default();
            let out = source
                .out_seconds
                .and_then(mark)
                .filter(|out| !out.is_zero());
            let end = match (out, file_duration) {
                (Some(out), Some(file)) => Some(out.min(file)),
                (out, file) => out.or(file),
            };
            if let Some(end) = end {
                if end <= start {
                    return Self {
                        start: Duration::ZERO,
                        length: file_duration,
                    };
                }
            }
            Self {
                start,
                length: end.map(|end| end - start),
            }
        }

        fn decoder_at<S: Source>(
            self,
            mut decoder: S,
            offset: Duration,
        ) -> Result<S, &'static str> {
            let Some(position) = self.start.checked_add(offset) else {
                return Err("Could not seek within this audio file.");
            };
            if !position.is_zero() {
                decoder
                    .try_seek(position)
                    .map_err(|_| "Could not seek within this audio file.")?;
            }
            Ok(decoder)
        }
    }

    fn open_decoder(path: &Path) -> Result<FileDecoder, &'static str> {
        if !path.is_absolute() {
            return Err("Premiere did not provide an absolute audio source path.");
        }
        let file = File::open(path).map_err(|_| "Could not open the audio source file.")?;
        Decoder::try_from(file).map_err(|_| "This audio file could not be decoded.")
    }

    #[cfg(windows)]
    use {
        rodio::{DeviceSinkBuilder, MixerDeviceSink, Player},
        std::sync::atomic::AtomicBool,
    };

    #[cfg(windows)]
    struct Output {
        player: Player,
        _device: MixerDeviceSink,
        failed: Arc<AtomicBool>,
    }

    #[cfg(windows)]
    pub(super) struct Playback {
        source: AudioPreviewSource,
        range: Range,
        offset: Duration,
        output: Option<Output>,
    }

    #[cfg(windows)]
    impl Playback {
        pub fn open(
            source: AudioPreviewSource,
            current: impl Fn() -> bool,
        ) -> Result<Self, &'static str> {
            let decoder = open_decoder(&source.path)?;
            let range = Range::from_marks(&source, decoder.total_duration());
            let mut playback = Self {
                source,
                range,
                offset: Duration::ZERO,
                output: None,
            };
            playback.start_at(decoder, Duration::ZERO, true, current)?;
            Ok(playback)
        }

        fn start_at(
            &mut self,
            decoder: FileDecoder,
            offset: Duration,
            play: bool,
            current: impl Fn() -> bool,
        ) -> Result<(), &'static str> {
            self.output = None;
            self.offset = offset;
            if self.range.length == Some(offset) || !current() {
                return Ok(());
            }
            let decoder = self.range.decoder_at(decoder, offset)?;
            if !current() {
                return Ok(());
            }
            let failed = Arc::new(AtomicBool::new(false));
            let device_failed = failed.clone();
            let mut device = DeviceSinkBuilder::from_default_device()
                .and_then(|builder| {
                    builder
                        .with_error_callback(move |_| {
                            device_failed.store(true, Ordering::Release);
                        })
                        .open_sink_or_fallback()
                })
                .map_err(|_| "Could not open the default audio output.")?;
            device.log_on_drop(false);
            let player = Player::connect_new(device.mixer());
            player.pause();
            if let Some(length) = self.range.length {
                player.append(decoder.take_duration(length - offset));
            } else {
                player.append(decoder);
            }
            if play && current() {
                player.play();
            }
            self.output = Some(Output {
                player,
                _device: device,
                failed,
            });
            Ok(())
        }

        pub fn playing(&self) -> bool {
            self.output
                .as_ref()
                .is_some_and(|output| !output.player.is_paused())
        }

        pub fn toggle(&mut self, current: impl Fn() -> bool) -> Result<(), &'static str> {
            self.snapshot()?;
            if let Some(output) = &self.output {
                if output.player.is_paused() {
                    if current() {
                        output.player.play();
                    }
                } else {
                    output.player.pause();
                }
                Ok(())
            } else {
                self.start_at(
                    open_decoder(&self.source.path)?,
                    Duration::ZERO,
                    true,
                    current,
                )
            }
        }

        pub fn seek(
            &mut self,
            fraction: f32,
            current: impl Fn() -> bool,
        ) -> Result<(), &'static str> {
            let Some(length) = self.range.length else {
                return Ok(());
            };
            self.snapshot()?;
            let play = self.playing();
            self.output = None;
            let offset = length.mul_f64(f64::from(fraction)).min(length);
            self.start_at(open_decoder(&self.source.path)?, offset, play, current)
        }

        pub fn snapshot(&mut self) -> Result<Snapshot, &'static str> {
            let mut position = self.offset;
            if let Some(output) = &self.output {
                if output.failed.load(Ordering::Acquire) {
                    return Err("The audio output became unavailable.");
                }
                position = position.saturating_add(output.player.get_pos());
                if output.player.empty() {
                    self.offset = self.range.length.unwrap_or(position);
                    self.output = None;
                    position = self.offset;
                }
            }
            Ok(Snapshot {
                position,
                duration: self.range.length,
                playing: self.playing(),
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Cursor;

        fn fixture_wav() -> Vec<u8> {
            let sample_rate = 8_000u32;
            let data_size = sample_rate * 2;
            let mut wav = b"RIFF".to_vec();
            wav.extend((36 + data_size).to_le_bytes());
            wav.extend(b"WAVEfmt ");
            wav.extend(16u32.to_le_bytes());
            wav.extend([1, 0, 1, 0]); // PCM, mono
            wav.extend(sample_rate.to_le_bytes());
            wav.extend((sample_rate * 2).to_le_bytes());
            wav.extend([2, 0, 16, 0]); // One 16-bit sample per frame
            wav.extend(b"data");
            wav.extend(data_size.to_le_bytes());
            for value in [0i16, 1_000, 2_000, 3_000] {
                for _ in 0..sample_rate / 4 {
                    wav.extend(value.to_le_bytes());
                }
            }
            wav
        }

        #[test]
        fn seeks_relative_to_the_marked_range_and_bounds_output_at_the_out_point() {
            let source = AudioPreviewSource {
                path: Default::default(),
                in_seconds: Some(0.25),
                out_seconds: Some(0.75),
            };
            let decode = || Decoder::try_from(Cursor::new(fixture_wav())).unwrap();
            let range = Range::from_marks(&source, decode().total_duration());
            assert_eq!(range.start, Duration::from_millis(250));
            assert_eq!(range.length, Some(Duration::from_millis(500)));

            let offset = Duration::from_millis(375);
            let samples: Vec<_> = range
                .decoder_at(decode(), offset)
                .unwrap()
                .take_duration(range.length.unwrap() - offset)
                .collect();
            assert!(!samples.is_empty());
            assert!(samples
                .iter()
                .all(|sample| (*sample - 2_000.0 / 32_768.0).abs() < 1e-5));
            // The duration filter can round the exclusive Out point down by one sample.
            assert!((999..=1_000).contains(&samples.len()));

            let start = range
                .decoder_at(decode(), Duration::ZERO)
                .unwrap()
                .next()
                .unwrap();
            assert!((start - 1_000.0 / 32_768.0).abs() < 1e-5);
        }

        #[test]
        fn uses_file_bounds_for_unset_marks_and_clamps_past_eof() {
            let second = Duration::from_secs(1);
            let source = AudioPreviewSource {
                path: Default::default(),
                in_seconds: Some(0.25),
                out_seconds: Some(0.0),
            };
            assert_eq!(
                Range::from_marks(&source, Some(second)),
                Range {
                    start: Duration::from_millis(250),
                    length: Some(Duration::from_millis(750))
                },
            );
            let source = AudioPreviewSource {
                in_seconds: None,
                out_seconds: Some(2.0),
                ..source
            };
            assert_eq!(
                Range::from_marks(&source, Some(second)),
                Range {
                    start: Duration::ZERO,
                    length: Some(second)
                },
            );
        }
    }
}
