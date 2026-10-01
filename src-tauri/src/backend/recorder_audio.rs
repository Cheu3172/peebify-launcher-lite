// ------------ Recorder Audio Capture ------------
// Captures sound for the clip recorder through WASAPI: the desktop mix, the microphone,
// or a single app's audio. Each source becomes 48 kHz stereo packets and the Mixer lines
// them up on the video clock. Only runs on Windows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{Interface, Ref, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, ActivateAudioInterfaceAsync, EDataFlow,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;

const WAIT_MS: u32 = 100;
const POLL_SLEEP: Duration = Duration::from_millis(4);
const DEFAULT_CHECK: Duration = Duration::from_secs(1);
const REOPEN_FIRST: Duration = Duration::from_millis(250);
const REOPEN_MAX: Duration = Duration::from_secs(2);
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const EXTENSIBLE_EXTRA: u16 = 22;
const SIDE_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;
const LIMIT_KNEE: f32 = 0.8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Desktop,
    Microphone,
    Process(u32),
}

impl SourceKind {
    pub fn label(&self) -> String {
        match self {
            SourceKind::Desktop => "desktop".to_string(),
            SourceKind::Microphone => "microphone".to_string(),
            SourceKind::Process(pid) => format!("process {pid}"),
        }
    }
}

pub struct Packet {
    pub source: usize,
    pub pcm: Vec<i16>,
    pub qpc_100ns: u64,
}

pub struct AudioCapture {
    stop: Arc<AtomicBool>,
    name: String,
    ready: Receiver<Result<(), String>>,
}

impl AudioCapture {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn wait_ready(&self, deadline: Instant) -> Option<Result<(), String>> {
        let wait = deadline.saturating_duration_since(Instant::now());
        match self.ready.recv_timeout(wait) {
            Ok(result) => Some(result),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                Some(Err("the audio thread ended before it started".to_string()))
            }
        }
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn start(
    index: usize,
    kind: SourceKind,
    name: String,
    tx: Sender<Packet>,
) -> Result<AudioCapture, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let (ready_tx, ready) = std::sync::mpsc::channel();
    let thread_name = name.clone();

    std::thread::Builder::new()
        .name(format!("overlay-audio-{index}"))
        .spawn(move || {
            let name = thread_name;
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            match pump(index, kind, &tx, &thread_stop, &ready_tx) {
                Ok(()) => log::info!("recorder: {name} audio ended"),
                Err(e) => {
                    log::warn!("recorder: {name} audio stopped ({e})");
                    let _ = ready_tx.send(Err(e));
                }
            }
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        })
        .map_err(|e| format!("could not start the audio thread: {e}"))?;

    Ok(AudioCapture { stop, name, ready })
}

fn capture_error(loopback: u32, what: &str, e: &windows::core::Error) -> String {
    if loopback == 0 && e.code() == E_ACCESSDENIED {
        "Windows is blocking microphone access for desktop apps.".to_string()
    } else {
        format!("{what}: {e}")
    }
}

struct Event(HANDLE);

impl Event {
    fn new(manual_reset: bool) -> Result<Self, String> {
        unsafe { CreateEventW(None, manual_reset, false, PCWSTR::null()) }
            .map(Event)
            .map_err(|e| format!("could not create the audio event: {e}"))
    }
}

unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[windows_core::implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationDone {
    signal: Arc<Event>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationDone_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        unsafe { SetEvent(self.signal.0) }
    }
}

fn stereo_format(tag: u16, bits: u16) -> WAVEFORMATEX {
    let block_align = CHANNELS as u16 * bits / 8;
    WAVEFORMATEX {
        wFormatTag: tag,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: bits,
        cbSize: 0,
    }
}

unsafe fn process_loopback_client(pid: u32) -> Result<IAudioClient, String> {
    let signal = Arc::new(Event::new(true)?);

    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };

    let mut activation = std::mem::ManuallyDrop::new(PROPVARIANT::default());
    {
        let inner = &mut activation.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob.cbSize = std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
        inner.Anonymous.blob.pBlobData = &mut params as *mut _ as *mut u8;
    }

    let handler = ActivationDone {
        signal: signal.clone(),
    };
    let handler: IActivateAudioInterfaceCompletionHandler = handler.into();

    let operation = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&*activation),
        &handler,
    )
    .map_err(|e| format!("this build of Windows cannot capture one app's audio: {e}"))?;

    if WaitForSingleObject(signal.0, 5_000) != WAIT_OBJECT_0 {
        return Err("Windows did not answer the audio activation request.".to_string());
    }

    let mut status = windows::core::HRESULT(0);
    let mut activated: Option<windows::core::IUnknown> = None;
    operation
        .GetActivateResult(&mut status, &mut activated)
        .map_err(|e| format!("could not open that app's audio: {e}"))?;
    status
        .ok()
        .map_err(|e| format!("that app's audio could not be captured: {e}"))?;
    activated
        .ok_or_else(|| "Windows returned no audio client for that app.".to_string())?
        .cast::<IAudioClient>()
        .map_err(|e| format!("could not open that app's audio: {e}"))
}

fn flow_of(kind: SourceKind) -> EDataFlow {
    if matches!(kind, SourceKind::Microphone) {
        eCapture
    } else {
        eRender
    }
}

unsafe fn device_id(device: &IMMDevice) -> Option<String> {
    let id = device.GetId().ok()?;
    let text = id.to_string().ok();
    CoTaskMemFree(Some(id.0 as *const _));
    text
}

unsafe fn default_device_id(enumerator: &IMMDeviceEnumerator, kind: SourceKind) -> Option<String> {
    let device = enumerator
        .GetDefaultAudioEndpoint(flow_of(kind), eConsole)
        .ok()?;
    device_id(&device)
}

unsafe fn converted_client(device: &IMMDevice, loopback: u32) -> Result<IAudioClient, String> {
    let client: IAudioClient = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|e| capture_error(loopback, "could not open the audio device", &e))?;
    let format = stereo_format(WAVE_FORMAT_IEEE_FLOAT, 32);
    client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            loopback | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            10_000_000,
            0,
            &format,
            None,
        )
        .map_err(|e| capture_error(loopback, "could not start capturing", &e))?;
    Ok(client)
}

unsafe fn native_client(device: &IMMDevice, loopback: u32) -> Result<(IAudioClient, Source), String> {
    let client: IAudioClient = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|e| capture_error(loopback, "could not open the audio device", &e))?;
    let mix = client
        .GetMixFormat()
        .map_err(|e| format!("could not read the audio format: {e}"))?;
    let source = describe(mix);
    let result = client.Initialize(AUDCLNT_SHAREMODE_SHARED, loopback, 10_000_000, 0, mix, None);
    CoTaskMemFree(Some(mix as *const _));
    result.map_err(|e| capture_error(loopback, "could not start capturing", &e))?;
    Ok((client, source))
}

unsafe fn endpoint_client(
    device: &IMMDevice,
    kind: SourceKind,
) -> Result<(IAudioClient, Source), String> {
    let loopback = if matches!(kind, SourceKind::Microphone) {
        0
    } else {
        AUDCLNT_STREAMFLAGS_LOOPBACK
    };
    match converted_client(device, loopback) {
        Ok(client) => Ok((client, Source::stereo(true))),
        Err(e) => {
            log::info!(
                "recorder: Windows would not convert {} audio to stereo ({e}), so it is mixed down here",
                kind.label()
            );
            native_client(device, loopback)
        }
    }
}

struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    source: Source,
    watch: Option<(IMMDeviceEnumerator, String)>,
    signal: Option<Event>,
}

unsafe fn open_stream(kind: SourceKind) -> Result<Stream, String> {
    let (client, source, watch, signal) = match kind {
        SourceKind::Process(pid) => {
            let client = process_loopback_client(pid)?;
            let format = stereo_format(WAVE_FORMAT_PCM as u16, 16);
            let signal = Event::new(false)?;
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                    0,
                    0,
                    &format,
                    None,
                )
                .map_err(|e| format!("could not start that app's audio: {e}"))?;
            client
                .SetEventHandle(signal.0)
                .map_err(|e| format!("could not arm that app's audio: {e}"))?;
            (client, Source::stereo(false), None, Some(signal))
        }
        _ => {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| format!("no audio device enumerator: {e}"))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(flow_of(kind), eConsole)
                .map_err(|e| format!("no default audio device: {e}"))?;
            let id = device_id(&device);
            let (client, source) = endpoint_client(&device, kind)?;
            (client, source, id.map(|id| (enumerator, id)), None)
        }
    };

    let capture: IAudioCaptureClient = client
        .GetService()
        .map_err(|e| format!("could not get the capture client: {e}"))?;
    client
        .Start()
        .map_err(|e| format!("could not start capturing: {e}"))?;

    Ok(Stream {
        client,
        capture,
        source,
        watch,
        signal,
    })
}

enum Outcome {
    Stopped,
    Closed,
    Moved,
    Lost(String),
}

unsafe fn run(
    stream: &Stream,
    kind: SourceKind,
    index: usize,
    tx: &Sender<Packet>,
    stop: &AtomicBool,
    delivered: &mut u64,
) -> Outcome {
    let mut resampler = Resampler::new(stream.source.rate, SAMPLE_RATE);
    let mut checked = Instant::now();

    while !stop.load(Ordering::Acquire) {
        if let Some((enumerator, id)) = &stream.watch {
            if checked.elapsed() >= DEFAULT_CHECK {
                checked = Instant::now();
                if default_device_id(enumerator, kind).is_some_and(|current| &current != id) {
                    return Outcome::Moved;
                }
            }
        }

        match &stream.signal {
            Some(signal) => {
                if WaitForSingleObject(signal.0, WAIT_MS) != WAIT_OBJECT_0 {
                    if let Err(e) = stream.capture.GetNextPacketSize() {
                        return Outcome::Lost(e.to_string());
                    }
                    continue;
                }
            }
            None => match stream.capture.GetNextPacketSize() {
                Ok(0) => {
                    std::thread::sleep(POLL_SLEEP);
                    continue;
                }
                Ok(_) => {}
                Err(e) => return Outcome::Lost(e.to_string()),
            },
        }

        loop {
            match stream.capture.GetNextPacketSize() {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => return Outcome::Lost(e.to_string()),
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut position = 0u64;
            if let Err(e) = stream.capture.GetBuffer(
                &mut data,
                &mut frames,
                &mut flags,
                None,
                Some(&mut position),
            ) {
                return Outcome::Lost(e.to_string());
            }

            if frames > 0 {
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                let stereo = if silent {
                    vec![0i16; frames as usize * CHANNELS as usize]
                } else {
                    to_stereo_i16(data, frames, &stream.source)
                };
                let pcm = resampler.push(&stereo);
                if !pcm.is_empty() {
                    *delivered += 1;
                    if tx
                        .send(Packet {
                            source: index,
                            pcm,
                            qpc_100ns: position,
                        })
                        .is_err()
                    {
                        let _ = stream.capture.ReleaseBuffer(frames);
                        return Outcome::Closed;
                    }
                }
            }

            let _ = stream.capture.ReleaseBuffer(frames);
        }
    }
    Outcome::Stopped
}

fn wait_unless_stopped(stop: &AtomicBool, duration: Duration) -> bool {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !stop.load(Ordering::Acquire)
}

unsafe fn reopen(kind: SourceKind, stop: &AtomicBool) -> Option<Stream> {
    let mut delay = REOPEN_FIRST;
    let mut reported = false;
    loop {
        if !wait_unless_stopped(stop, delay) {
            return None;
        }
        match open_stream(kind) {
            Ok(stream) => return Some(stream),
            Err(e) => {
                if !reported {
                    reported = true;
                    log::warn!(
                        "recorder: {} audio could not be reopened yet ({e}), still trying",
                        kind.label()
                    );
                }
                delay = (delay * 2).min(REOPEN_MAX);
            }
        }
    }
}

fn pump(
    index: usize,
    kind: SourceKind,
    tx: &Sender<Packet>,
    stop: &AtomicBool,
    ready: &Sender<Result<(), String>>,
) -> Result<(), String> {
    unsafe {
        let mut stream = open_stream(kind)?;
        let _ = ready.send(Ok(()));
        let mut delivered = 0u64;

        loop {
            let outcome = run(&stream, kind, index, tx, stop, &mut delivered);
            let _ = stream.client.Stop();
            match outcome {
                Outcome::Stopped => break,
                Outcome::Closed => return Ok(()),
                Outcome::Lost(reason) if matches!(kind, SourceKind::Process(_)) => {
                    return Err(format!("the app's audio stream went away: {reason}"));
                }
                Outcome::Lost(reason) => log::warn!(
                    "recorder: {} audio device went away ({reason}), reopening",
                    kind.label()
                ),
                Outcome::Moved => log::info!(
                    "recorder: the default {} device changed, following it",
                    kind.label()
                ),
            }
            drop(stream);
            match reopen(kind, stop) {
                Some(next) => {
                    log::info!("recorder: {} audio reopened", kind.label());
                    stream = next;
                }
                None => break,
            }
        }

        if delivered == 0 {
            log::warn!(
                "recorder: {} produced no audio at all during this recording",
                kind.label()
            );
        }
    }
    Ok(())
}

struct Source {
    rate: u32,
    channels: u32,
    float: bool,
    bytes: u16,
    gains: Vec<(f32, f32)>,
    limit: bool,
}

impl Source {
    fn stereo(float: bool) -> Self {
        Source {
            rate: SAMPLE_RATE,
            channels: CHANNELS,
            float,
            bytes: if float { 4 } else { 2 },
            gains: channel_gains(0, CHANNELS as usize),
            limit: false,
        }
    }
}

fn default_mask(channels: usize) -> u32 {
    match channels {
        3 => 0x7,
        4 => 0x33,
        5 => 0x37,
        6 => 0x3F,
        7 => 0x70F,
        8 => 0x63F,
        _ => 0x3,
    }
}

fn speaker_gain(bit: u32) -> (f32, f32) {
    const FRONT_LEFT: u32 = 0x1 | 0x40;
    const FRONT_RIGHT: u32 = 0x2 | 0x80;
    const REAR_LEFT: u32 = 0x10 | 0x200 | 0x1000 | 0x8000;
    const REAR_RIGHT: u32 = 0x20 | 0x400 | 0x4000 | 0x20000;
    const CENTER: u32 = 0x4 | 0x100 | 0x800 | 0x2000 | 0x10000;
    if bit & FRONT_LEFT != 0 {
        (1.0, 0.0)
    } else if bit & FRONT_RIGHT != 0 {
        (0.0, 1.0)
    } else if bit & REAR_LEFT != 0 {
        (SIDE_GAIN, 0.0)
    } else if bit & REAR_RIGHT != 0 {
        (0.0, SIDE_GAIN)
    } else if bit & CENTER != 0 {
        (SIDE_GAIN, SIDE_GAIN)
    } else {
        (0.0, 0.0)
    }
}

fn channel_gains(mask: u32, channels: usize) -> Vec<(f32, f32)> {
    if channels <= 1 {
        return vec![(1.0, 1.0)];
    }
    let mask = if mask.count_ones() as usize >= channels {
        mask
    } else {
        default_mask(channels)
    };
    let mut bits = (0..32u32)
        .map(|shift| 1u32 << shift)
        .filter(|bit| mask & bit != 0);
    (0..channels)
        .map(|_| bits.next().map(speaker_gain).unwrap_or((0.0, 0.0)))
        .collect()
}

fn describe(format: *const WAVEFORMATEX) -> Source {
    let header = unsafe { std::ptr::read_unaligned(format) };
    let channels = header.nChannels.max(1);
    let extensible =
        header.wFormatTag == WAVE_FORMAT_EXTENSIBLE && header.cbSize >= EXTENSIBLE_EXTRA;
    let (float, mask) = if extensible {
        let extended = unsafe { std::ptr::read_unaligned(format as *const WAVEFORMATEXTENSIBLE) };
        let sub = extended.SubFormat;
        let mask = extended.dwChannelMask;
        (sub.data1 == WAVE_FORMAT_IEEE_FLOAT as u32, mask)
    } else {
        (header.wFormatTag == WAVE_FORMAT_IEEE_FLOAT, 0)
    };
    let bytes = if header.nBlockAlign >= channels {
        header.nBlockAlign / channels
    } else {
        header.wBitsPerSample / 8
    };
    Source {
        rate: header.nSamplesPerSec,
        channels: channels as u32,
        float,
        bytes,
        gains: channel_gains(mask, channels as usize),
        limit: channels > 2,
    }
}

fn to_stereo_i16(data: *const u8, frames: u32, source: &Source) -> Vec<i16> {
    let channels = source.channels.max(1) as usize;
    let mut out = Vec::with_capacity(frames as usize * CHANNELS as usize);

    for frame in 0..frames as usize {
        let mut left = 0f32;
        let mut right = 0f32;
        for channel in 0..channels {
            let index = frame * channels + channel;
            let value = unsafe { sample_at(data, index, source) };
            let (to_left, to_right) = source.gains.get(channel).copied().unwrap_or((0.0, 0.0));
            left += value * to_left;
            right += value * to_right;
        }
        if source.limit {
            left = soft_limit(left);
            right = soft_limit(right);
        }
        out.push(clamp_i16(left));
        out.push(clamp_i16(right));
    }
    out
}

unsafe fn sample_at(data: *const u8, index: usize, source: &Source) -> f32 {
    let at = data.add(index * source.bytes as usize);
    match (source.float, source.bytes) {
        (true, 4) => std::ptr::read_unaligned(at as *const f32),
        (true, 8) => std::ptr::read_unaligned(at as *const f64) as f32,
        (false, 1) => (*at as f32 - 128.0) / 128.0,
        (false, 2) => std::ptr::read_unaligned(at as *const i16) as f32 / 32768.0,
        (false, 3) => {
            let raw = i32::from_le_bytes([0, *at, *at.add(1), *at.add(2)]);
            raw as f32 / 2_147_483_648.0
        }
        (false, 4) => std::ptr::read_unaligned(at as *const i32) as f32 / 2_147_483_648.0,
        _ => 0.0,
    }
}

fn soft_limit(value: f32) -> f32 {
    let magnitude = value.abs();
    if magnitude <= LIMIT_KNEE {
        return value;
    }
    let over = (magnitude - LIMIT_KNEE) / (1.0 - LIMIT_KNEE);
    (LIMIT_KNEE + (1.0 - LIMIT_KNEE) * over / (1.0 + over)).copysign(value)
}

fn clamp_i16(value: f32) -> i16 {
    (value.clamp(-1.0, 1.0) * 32767.0) as i16
}

struct Resampler {
    from: u32,
    to: u32,
    position: f64,
    tail: [i16; 2],
    primed: bool,
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Self {
            from: from.max(1),
            to,
            position: 0.0,
            tail: [0, 0],
            primed: false,
        }
    }

    fn push(&mut self, input: &[i16]) -> Vec<i16> {
        if self.from == self.to {
            return input.to_vec();
        }
        let frames = input.len() / 2;
        if frames == 0 {
            return Vec::new();
        }
        let step = self.from as f64 / self.to as f64;
        let mut out = Vec::with_capacity((frames as f64 / step) as usize * 2 + 4);

        while self.position < frames as f64 {
            let index = self.position as usize;
            let frac = self.position - index as f64;
            for channel in 0..2 {
                let previous = if index == 0 {
                    if self.primed {
                        self.tail[channel]
                    } else {
                        input[channel]
                    }
                } else {
                    input[(index - 1) * 2 + channel]
                };
                let current = input[index * 2 + channel];
                let value = previous as f64 + (current as f64 - previous as f64) * frac;
                out.push(value as i16);
            }
            self.position += step;
        }

        self.position -= frames as f64;
        self.tail = [input[(frames - 1) * 2], input[(frames - 1) * 2 + 1]];
        self.primed = true;
        out
    }
}

const CURSOR_TOLERANCE_FRAMES: u64 = (SAMPLE_RATE / 50) as u64;

pub struct Mixer {
    origin: u64,
    base: u64,
    accumulator: Vec<i32>,
    horizon: usize,
    contributed: Vec<u64>,
    cursors: Vec<Option<u64>>,
}

impl Mixer {
    pub fn new(origin: u64, sources: usize) -> Self {
        Self {
            origin,
            base: 0,
            accumulator: Vec::new(),
            horizon: (SAMPLE_RATE * 5) as usize,
            contributed: vec![0; sources],
            cursors: vec![None; sources],
        }
    }

    pub fn contributed(&self) -> &[u64] {
        &self.contributed
    }

    pub fn add(&mut self, packet: &Packet) {
        let Some(offset) = packet.qpc_100ns.checked_sub(self.origin) else {
            return;
        };
        let clock_start = offset * SAMPLE_RATE as u64 / 10_000_000;
        let frames = packet.pcm.len() / CHANNELS as usize;
        if frames == 0 {
            return;
        }
        let cursor = self.cursors.get(packet.source).copied().flatten();
        let start = match cursor {
            Some(next) if next.abs_diff(clock_start) <= CURSOR_TOLERANCE_FRAMES => next,
            _ => clock_start,
        };
        if let Some(slot) = self.cursors.get_mut(packet.source) {
            *slot = Some(start + frames as u64);
        }

        let (start, skip) = if start < self.base {
            let behind = (self.base - start) as usize;
            if behind >= frames {
                return;
            }
            (self.base, behind)
        } else {
            (start, 0)
        };

        let at = (start - self.base) as usize;
        if at > self.horizon {
            return;
        }
        let needed = (at + (frames - skip)) * CHANNELS as usize;
        if self.accumulator.len() < needed {
            self.accumulator.resize(needed, 0);
        }
        for frame in skip..frames {
            for channel in 0..CHANNELS as usize {
                let target = (at + frame - skip) * CHANNELS as usize + channel;
                self.accumulator[target] += packet.pcm[frame * CHANNELS as usize + channel] as i32;
            }
        }
        if let Some(count) = self.contributed.get_mut(packet.source) {
            *count += (frames - skip) as u64;
        }
    }

    pub fn take(&mut self, up_to: u64) -> Vec<i16> {
        if up_to <= self.base {
            return Vec::new();
        }
        let frames = (up_to - self.base) as usize;
        let samples = frames * CHANNELS as usize;
        if self.accumulator.len() < samples {
            self.accumulator.resize(samples, 0);
        }
        let out: Vec<i16> = self
            .accumulator
            .drain(..samples)
            .map(|v| v.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
            .collect();
        self.base = up_to;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(source: usize, qpc: u64, first: i16, frames: usize) -> Packet {
        let mut pcm = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = first.wrapping_add(i as i16);
            pcm.push(v);
            pcm.push(v);
        }
        Packet {
            source,
            pcm,
            qpc_100ns: qpc,
        }
    }

    #[test]
    fn jittered_packets_stay_contiguous() {
        let origin = 1_000_000;
        let mut mixer = Mixer::new(origin, 1);
        let jitter = [0i64, 37, -41, 12, -8, 55, -60, 3, 29, -17];
        for i in 0..200usize {
            let nominal = origin + i as u64 * 100_000;
            let qpc = (nominal as i64 + jitter[i % jitter.len()]) as u64;
            mixer.add(&packet(0, qpc, (i * 480) as i16, 480));
        }
        let out = mixer.take(200 * 480);
        assert_eq!(out.len(), 200 * 480 * 2);
        for (frame, chunk) in out.chunks(2).enumerate() {
            assert_eq!(chunk[0], frame as i16, "frame {frame} left");
            assert_eq!(chunk[1], frame as i16, "frame {frame} right");
        }
    }

    #[test]
    fn a_real_gap_is_filled_with_silence() {
        let origin = 0;
        let mut mixer = Mixer::new(origin, 1);
        mixer.add(&packet(0, 0, 1000, 480));
        mixer.add(&packet(0, 1_000_000, 2000, 480));
        let out = mixer.take(4800 + 480);
        assert_eq!(out[0], 1000);
        assert_eq!(out[479 * 2], 1000 + 479);
        assert!(out[480 * 2..4800 * 2].iter().all(|v| *v == 0));
        assert_eq!(out[4800 * 2], 2000);
    }

    #[test]
    fn sources_keep_independent_cursors_and_sum() {
        let mut mixer = Mixer::new(0, 2);
        mixer.add(&packet(0, 0, 100, 480));
        mixer.add(&packet(1, 25, 5, 480));
        mixer.add(&packet(0, 100_000 - 30, 580, 480));
        mixer.add(&packet(1, 100_000 + 40, 485, 480));
        let out = mixer.take(960);
        for frame in 0..960usize {
            assert_eq!(
                out[frame * 2],
                (100 + frame) as i16 + (5 + frame) as i16,
                "frame {frame}"
            );
        }
    }

    #[test]
    fn resampler_passthrough_is_identity() {
        let mut r = Resampler::new(48_000, 48_000);
        let input: Vec<i16> = (0..960).collect();
        assert_eq!(r.push(&input), input);
    }

    fn float_frames(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn surround_source(mask: u32) -> Source {
        Source {
            rate: SAMPLE_RATE,
            channels: 8,
            float: true,
            bytes: 4,
            gains: channel_gains(mask, 8),
            limit: true,
        }
    }

    #[test]
    fn seven_one_keeps_sides_apart_and_drops_lfe() {
        let gains = channel_gains(0x63F, 8);
        assert_eq!(gains[0], (1.0, 0.0));
        assert_eq!(gains[1], (0.0, 1.0));
        assert_eq!(gains[2], (SIDE_GAIN, SIDE_GAIN));
        assert_eq!(gains[3], (0.0, 0.0));
        assert_eq!(gains[4], (SIDE_GAIN, 0.0));
        assert_eq!(gains[5], (0.0, SIDE_GAIN));
        assert_eq!(gains[6], (SIDE_GAIN, 0.0));
        assert_eq!(gains[7], (0.0, SIDE_GAIN));
    }

    #[test]
    fn missing_mask_falls_back_to_the_standard_layout() {
        assert_eq!(channel_gains(0, 8), channel_gains(0x63F, 8));
        assert_eq!(channel_gains(0, 6), channel_gains(0x3F, 6));
        assert_eq!(channel_gains(0, 1), vec![(1.0, 1.0)]);
    }

    #[test]
    fn left_surround_stays_on_the_left() {
        let source = surround_source(0x63F);
        let data = float_frames(&[0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0]);
        let out = to_stereo_i16(data.as_ptr(), 1, &source);
        assert!(out[0] > 0);
        assert_eq!(out[1], 0);
    }

    #[test]
    fn front_only_content_keeps_its_level() {
        let source = surround_source(0x63F);
        let data = float_frames(&[0.5, 0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let out = to_stereo_i16(data.as_ptr(), 1, &source);
        assert_eq!(out[0], clamp_i16(0.5));
        assert_eq!(out[1], clamp_i16(0.25));
    }

    #[test]
    fn loud_surround_is_limited_not_clipped() {
        let source = surround_source(0x63F);
        let data = float_frames(&[1.0; 8]);
        let out = to_stereo_i16(data.as_ptr(), 1, &source);
        assert!(out[0] < i16::MAX);
        assert!(out[0] > clamp_i16(LIMIT_KNEE));
    }

    #[test]
    fn soft_limit_is_transparent_below_the_knee_and_bounded_above() {
        assert_eq!(soft_limit(0.5), 0.5);
        assert_eq!(soft_limit(-0.5), -0.5);
        assert!(soft_limit(3.0) < 1.0);
        assert!(soft_limit(-3.0) > -1.0);
        assert!(soft_limit(0.9) < soft_limit(1.2));
    }

    #[test]
    fn packed_24_bit_samples_decode() {
        let source = Source {
            rate: SAMPLE_RATE,
            channels: 2,
            float: false,
            bytes: 3,
            gains: channel_gains(0x3, 2),
            limit: false,
        };
        let data = [0x00u8, 0x00, 0x40, 0x00, 0x00, 0xC0];
        let out = to_stereo_i16(data.as_ptr(), 1, &source);
        assert_eq!(out[0], clamp_i16(0.5));
        assert_eq!(out[1], clamp_i16(-0.5));
    }

    fn extensible(bits: u16, channels: u16, mask: u32, sub: u32) -> WAVEFORMATEXTENSIBLE {
        let mut format = WAVEFORMATEXTENSIBLE::default();
        format.Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
        format.Format.nChannels = channels;
        format.Format.nSamplesPerSec = 44_100;
        format.Format.wBitsPerSample = bits;
        format.Format.nBlockAlign = channels * bits / 8;
        format.Format.cbSize = EXTENSIBLE_EXTRA;
        format.dwChannelMask = mask;
        format.SubFormat = windows::core::GUID::from_values(
            sub,
            0,
            0x10,
            [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
        );
        format
    }

    #[test]
    fn describe_reads_the_extensible_subformat() {
        let float = extensible(32, 8, 0x63F, 3);
        let source = describe(&float as *const _ as *const WAVEFORMATEX);
        assert!(source.float);
        assert_eq!(source.bytes, 4);
        assert_eq!(source.channels, 8);
        assert_eq!(source.rate, 44_100);
        assert!(source.limit);
        assert_eq!(source.gains, channel_gains(0x63F, 8));

        let integer = extensible(32, 2, 0x3, 1);
        let source = describe(&integer as *const _ as *const WAVEFORMATEX);
        assert!(!source.float);
        assert_eq!(source.bytes, 4);
        assert!(!source.limit);

        let packed = extensible(24, 2, 0x3, 1);
        let source = describe(&packed as *const _ as *const WAVEFORMATEX);
        assert!(!source.float);
        assert_eq!(source.bytes, 3);
    }

    #[test]
    fn capture_handle_stops_its_thread_when_dropped() {
        let stop = Arc::new(AtomicBool::new(false));
        let (_ready_tx, ready) = std::sync::mpsc::channel();
        drop(AudioCapture {
            stop: stop.clone(),
            name: "desktop".to_string(),
            ready,
        });
        assert!(stop.load(Ordering::Acquire));
    }
}
