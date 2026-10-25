//! The audio output: device choice, stream formats, exclusive access on the
//! Mac, and the stream itself.

use crate::mixer::Command;
use crate::records::EngineError;
use crate::render::{
    clamp16_from_float, exact_f32_to_i16, exact_f32_to_i24, render_bitperfect, render_f32, BailRamp,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::ErrorKind;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Output reconfiguration may overlap a user pause. Never restore a saved
/// pause boolean: that would undo the newer intent. Holds nest independently.
pub(super) struct OutputHold<'a>(&'a AtomicU32);
impl<'a> OutputHold<'a> {
    pub(super) fn new(holds: &'a AtomicU32) -> Self {
        holds.fetch_add(1, Ordering::AcqRel);
        Self(holds)
    }
}
impl Drop for OutputHold<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
pub(super) struct OutputControl {
    pub(super) commands: crossbeam_channel::Sender<Command>,
    pub(super) stream: Arc<Mutex<Option<cpal::Stream>>>,
    pub(super) rebuilding: Arc<AtomicBool>,
    /// A forced rebuild arrived while one was already running. The worker
    /// re-runs for it rather than dropping it — see `request_rebuild`.
    pub(super) rebuild_forced: Arc<AtomicBool>,
    pub(super) rebuild_lock: Arc<Mutex<()>>,
    pub(super) output_rate: Arc<AtomicU32>,
    pub(super) output_channels: Arc<AtomicU32>,
    pub(super) requested_rate: Arc<AtomicU32>,
    pub(super) requested_channels: Arc<AtomicU32>,
    pub(super) device_name: Arc<Mutex<String>>,
    pub(super) volume_bits: Arc<AtomicU32>,
    pub(super) sleep_gain_bits: Arc<AtomicU32>,
    pub(super) buffered: Arc<AtomicU64>,
    pub(super) callback_underruns: Arc<AtomicU64>,
    pub(super) output_rebuilds: Arc<AtomicU64>,
    pub(super) output_xruns: Arc<AtomicU64>,
    pub(super) output_peak: Arc<AtomicU32>,
    pub(super) flush_ring: Arc<AtomicBool>,
    pub(super) bail_flush: Arc<AtomicBool>,
    pub(super) output_paused: Arc<AtomicBool>,
    pub(super) output_holds: Arc<AtomicU32>,
    pub(super) pcm_mode: Arc<AtomicU32>,
    pub(super) active_pcm_mode: Arc<AtomicU32>,
    pub(super) bit_perfect_depth: Arc<AtomicU32>,
    pub(super) active_bit_perfect_depth: Arc<AtomicU32>,
    pub(super) bit_perfect_reason: Arc<Mutex<String>>,
    pub(super) prefer_usb: Arc<AtomicBool>,
}

impl OutputControl {
    pub(super) fn on_stream_error(&self, err: cpal::Error) {
        match err.kind() {
            ErrorKind::DeviceChanged => {
                // The route moved, and this has to force the rebuild. The
                // "is the existing stream still good?" comparison inside
                // `rebuild` cannot see a route change on iOS: cpal reports one
                // singleton "Default Device", so the name never changes, and
                // `choose_format` returns the AVAudioSession's own rate and
                // channel count because the app pins them, so those never
                // change either. Every term of that test is therefore true, and
                // a non-forced rebuild returned Ok having done nothing — which
                // is how unplugging AirPods left a stream whose device had
                // gone: silence, never recovered, with the mixer filling a ring
                // that nothing drains.
                log::info!("output route changed: {err}");
                self.request_rebuild(true);
            }
            ErrorKind::StreamInvalidated => {
                log::info!("output stream invalidated: {err}");
                self.request_rebuild(true);
            }
            ErrorKind::DeviceNotAvailable => {
                log::info!("output device unavailable: {err}");
                self.request_rebuild(true);
            }
            ErrorKind::Xrun => {
                self.output_xruns.fetch_add(1, Ordering::Relaxed);
                log::debug!("output xrun: {err}");
            }
            other => log::warn!("output stream error ({other:?}): {err}"),
        }
    }

    pub(super) fn request_rebuild(&self, force: bool) {
        if force {
            self.rebuild_forced.store(true, Ordering::Release);
        }
        if self.rebuilding.swap(true, Ordering::AcqRel) {
            // A rebuild is already in flight. A forced request must not simply
            // be dropped: it is the app saying the session is active now, and
            // the run already under way may well have built its unit before
            // that was true. The worker below re-runs for it.
            return;
        }
        // This worker owns the rebuild; the flag only had to survive the race
        // above, and leaving it set would earn a needless second pass.
        self.rebuild_forced.store(false, Ordering::Release);
        let ctrl = self.clone();
        let _ = std::thread::Builder::new()
            .name("native-core-output-rebuild".into())
            .spawn(move || {
                let mut force = force;
                loop {
                    for attempt in 0..20 {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        match ctrl.rebuild(force) {
                            Ok(()) => break,
                            Err(EngineError::NoOutputDevice) if attempt < 19 => {
                                log::info!("waiting for an output device…");
                            }
                            // Route changes can also race CoreAudio while the
                            // new unit is being created. Keep retrying
                            // transient stream setup failures; a single failed
                            // attempt must not leave the engine permanently
                            // without an output callback.
                            Err(e) if attempt < 19 => {
                                log::info!("output rebuild attempt {} failed: {e}", attempt + 1);
                            }
                            Err(e) => {
                                log::warn!("output rebuild failed: {e}");
                                break;
                            }
                        }
                    }
                    // A forced request that arrived while this ran is the app
                    // saying the session is up. Honour it before finishing, so
                    // the unit is never left built against a session that was
                    // down.
                    if !ctrl.rebuild_forced.swap(false, Ordering::AcqRel) {
                        break;
                    }
                    log::info!("re-running the output rebuild for a later route event");
                    force = true;
                }
                ctrl.rebuilding.store(false, Ordering::Release);
            });
    }

    pub(super) fn rebuild(&self, force: bool) -> Result<(), EngineError> {
        // Direct per-track preparation and route recovery share this lock.
        let _rebuild = self.rebuild_lock.lock().unwrap();
        let host = cpal::default_host();
        let device = pick_output_device(&host, self.prefer_usb.load(Ordering::Relaxed))
            .ok_or(EngineError::NoOutputDevice)?;
        // A route/DAC change is a *different* device even at the same format —
        // without the name check the comparison below would keep the old
        // stream on the old device.
        let device_changed = device.to_string() != *self.device_name.lock().unwrap();
        // Reopen a track's exact integer source format only if this route can
        // keep its clock and stereo layout. Other routes fall back to the
        // normal device format and source-rate conversion.
        let requested_rate = self.requested_rate.load(Ordering::Relaxed);
        let requested_channels = self.requested_channels.load(Ordering::Relaxed);
        let prefer_pcm16 = self.pcm_mode.load(Ordering::Relaxed) == 0;
        let requested_bp_depth = self.bit_perfect_depth.load(Ordering::Relaxed);
        let mut selected = if requested_bp_depth > 0 {
            match choose_bitperfect_format(
                &device,
                requested_rate,
                requested_channels,
                requested_bp_depth,
            )? {
                Some(format) => format,
                None => choose_format(&device, requested_rate, requested_channels, prefer_pcm16)?,
            }
        } else {
            choose_format(&device, requested_rate, requested_channels, prefer_pcm16)?
        };
        let mut bitperfect_fallback_reason = None;
        if requested_bp_depth > 0 && selected.bit_perfect_depth == 0 {
            bitperfect_fallback_reason = Some(
                "This route cannot open the exact source sample rate and stereo format".to_string(),
            );
        }
        if selected.bit_perfect_depth > 0 {
            if let Err(error) = acquire_output_exclusive(&device.to_string()) {
                bitperfect_fallback_reason = Some(format!("Exclusive output unavailable: {error}"));
                selected =
                    choose_format(&device, requested_rate, requested_channels, prefer_pcm16)?;
            }
        }
        let mut rate = selected.sample_rate;
        let mut channels = selected.channels;
        let prev_rate = self.output_rate.load(Ordering::Relaxed);
        let prev_ch = self.output_channels.load(Ordering::Relaxed);
        let prev_pcm16 = self.active_pcm_mode.load(Ordering::Relaxed) == 0;
        let prev_bit_perfect_depth = self.active_bit_perfect_depth.load(Ordering::Relaxed);
        if !force
            && !device_changed
            && rate == prev_rate
            && channels as u32 == prev_ch
            && selected.pcm16 == prev_pcm16
            && selected.bit_perfect_depth == prev_bit_perfect_depth
            && self.stream.lock().unwrap().is_some()
        {
            log::info!("output still {rate} Hz / {channels} ch — keeping stream");
            return Ok(());
        }

        let cap = rate as usize * 2;
        let (mut producer, consumer) = rtrb::RingBuffer::<f32>::new(cap);

        // Prove the replacement before giving anything up. Opening it first
        // means a failed attempt — a route still settling, the session not yet
        // re-activated, CoreAudio racing us — leaves the previous stream
        // exactly where it was, still draining the ring the mixer is still
        // filling. Tearing the old stream down first instead made every
        // transient failure permanent: no stream to play and a producer nothing
        // reads, so the mixer filled its ring once and then sat still for good.
        //
        // The new stream starts against an empty ring and simply outputs
        // silence until it is switched in below; the old one keeps playing real
        // audio throughout.
        let initial_stream = open_output_stream(
            &device,
            rate,
            channels,
            selected.pcm16,
            selected.bit_perfect_depth,
            consumer,
            self,
        );
        let mut stream = match initial_stream {
            Ok(stream) => stream,
            Err(_error) if selected.bit_perfect_depth > 0 => {
                bitperfect_fallback_reason =
                    Some("The route rejected the source-depth integer stream".to_string());
                release_output_exclusive();
                selected =
                    choose_format(&device, requested_rate, requested_channels, prefer_pcm16)?;
                rate = selected.sample_rate;
                channels = selected.channels;
                let (fallback_producer, consumer) = rtrb::RingBuffer::<f32>::new(cap);
                producer = fallback_producer;
                // Keep the exact source-rate request even when integer packing
                // fails; this still switches the DAC clock without claiming
                // bit-perfect delivery.
                open_output_stream(
                    &device,
                    selected.sample_rate,
                    selected.channels,
                    selected.pcm16,
                    0,
                    consumer,
                    self,
                )?
            }
            Err(error) => return Err(error),
        };
        if selected.bit_perfect_depth > 0
            && !output_physical_format_matches(
                &device.to_string(),
                rate,
                channels as u32,
                selected.bit_perfect_depth,
            )
        {
            bitperfect_fallback_reason =
                Some("CoreAudio did not retain the exact integer hardware format".to_string());
            release_output_exclusive();
            selected = choose_format(&device, requested_rate, requested_channels, prefer_pcm16)?;
            rate = selected.sample_rate;
            channels = selected.channels;
            let (fallback_producer, consumer) = rtrb::RingBuffer::<f32>::new(cap);
            producer = fallback_producer;
            stream =
                open_output_stream(&device, rate, channels, selected.pcm16, 0, consumer, self)?;
        }
        stream
            .play()
            .map_err(|e| EngineError::StreamInit(e.to_string()))?;
        #[cfg(target_os = "ios")]
        if self.output_paused.load(Ordering::Acquire) {
            stream
                .pause()
                .map_err(|e| EngineError::StreamInit(e.to_string()))?;
        }

        // Mute both units for the handover so
        // the outgoing ring's backlog — seconds of it — cannot double the
        // incoming audio while it drains.
        let _hold = OutputHold::new(&self.output_holds);
        self.flush_ring.store(true, Ordering::Release);
        let (acknowledged, acknowledgment) = crossbeam_channel::bounded(1);
        let generation = self.output_rebuilds.load(Ordering::Relaxed) + 1;
        if self
            .commands
            .send(Command::SetOutputFormat {
                rate,
                producer,
                generation,
                acknowledged: Some(acknowledged),
            })
            .is_err()
        {
            return Err(EngineError::NotStarted);
        }
        match acknowledgment.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(true) => {}
            Ok(false) => {
                return Err(EngineError::StreamInit(
                    "Superseded output generation".into(),
                ))
            }
            Err(_) => {
                return Err(EngineError::StreamInit(
                    "Mixer did not acknowledge the output change".into(),
                ))
            }
        }
        if selected.bit_perfect_depth == 0 && prev_bit_perfect_depth > 0 {
            let _ = self.commands.send(Command::SetBitPerfect(false));
        }
        // Hand the new stream in and drop the old one, whose device has gone.
        #[cfg(target_os = "ios")]
        let transport_result;
        let previous = {
            let mut output = self.stream.lock().unwrap();
            // Reconcile under the same lock as Play/Pause: a route rebuild
            // must not restart output for playback paused during its setup.
            #[cfg(target_os = "ios")]
            {
                let result = if self.output_paused.load(Ordering::Acquire) {
                    stream.pause()
                } else {
                    stream.play()
                };
                transport_result = result.map_err(|e| EngineError::StreamInit(e.to_string()));
            }
            output.replace(stream)
        };
        drop(previous);
        self.output_rate.store(rate, Ordering::Relaxed);
        self.output_channels
            .store(channels as u32, Ordering::Relaxed);
        self.active_pcm_mode
            .store(if selected.pcm16 { 0 } else { 1 }, Ordering::Relaxed);
        self.active_bit_perfect_depth
            .store(selected.bit_perfect_depth, Ordering::Relaxed);
        if let Some(reason) = bitperfect_fallback_reason {
            *self.bit_perfect_reason.lock().unwrap() = reason;
        }
        // A rebuild after a route change is a *different* device, so the name
        // is re-read here rather than left as the one the first start found.
        *self.device_name.lock().unwrap() = device.to_string();
        self.output_rebuilds.fetch_add(1, Ordering::Relaxed);
        log::info!("output rebuilt: {device} — {rate} Hz, {channels} ch");
        // The mixer has already switched rings. Keep its matching consumer
        // installed even if the route rejects the final transport operation.
        #[cfg(target_os = "ios")]
        transport_result?;
        Ok(())
    }
}

/// The format the host platform asked for, falling back to cpal's answer.
///
/// A hint of `0` means "no opinion" and takes cpal's figure. A requested
/// channel count of 1 is honoured rather than read as unset — a mono session
/// is a real configuration, and treating it as absent would hand the audio unit
/// a different channel count than the session believes it has.
/// The output device to open: the system default, unless the listener asked
/// to prefer a USB DAC and one is actually attached (upstream
/// `applyOutputRoute` / `setPreferredAudioDevice`, minus the Android routing
/// API — here the choice is which cpal device to open).
///
/// Name match rather than device id: cpal gives no stable identifier across
/// route changes, and "USB" in the product name is what every class-compliant
/// DAC reports through CoreAudio. No match falls back to the default rather
/// than failing — an absent DAC must never mean silence.
pub(super) fn pick_output_device(host: &cpal::Host, prefer_usb: bool) -> Option<cpal::Device> {
    let fallback = host.default_output_device();
    if !prefer_usb {
        return fallback;
    }
    // cpal 0.18 has no `Device::name` — the human name is the `Display`
    // impl, same string the pipeline readout already reports.
    let usb = host
        .output_devices()
        .ok()
        .and_then(|mut devices| devices.find(|d| d.to_string().to_lowercase().contains("usb")));
    match usb {
        Some(d) => {
            log::info!("preferring USB output: {d}");
            Some(d)
        }
        None => {
            log::info!("USB DAC preferred but none attached; using system default");
            fallback
        }
    }
}

pub(super) fn is_lossless_pcm_codec(codec: &str) -> bool {
    matches!(codec, "FLAC" | "ALAC" | "PCM" | "PCM Float")
}

#[cfg(target_os = "macos")]
pub(super) mod macos_exclusive {
    use std::ffi::c_void;
    use std::ptr::{null, NonNull};
    use std::sync::{Mutex, OnceLock};

    use objc2_core_audio::{
        kAudioDevicePropertyHogMode, kAudioDevicePropertyStreamConfiguration,
        kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain, kAudioObjectPropertyName,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
        kAudioStreamPropertyAvailablePhysicalFormats, kAudioStreamPropertyPhysicalFormat,
        AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
        AudioObjectPropertyAddress, AudioObjectSetPropertyData, AudioStreamRangedDescription,
    };
    use objc2_core_audio_types::{
        kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatFlagIsSignedInteger,
        kAudioFormatLinearPCM, AudioStreamBasicDescription,
    };
    use objc2_core_foundation::{CFRetained, CFString};

    static HOGGED_DEVICE: OnceLock<Mutex<Option<AudioObjectID>>> = OnceLock::new();

    fn hogged_device() -> &'static Mutex<Option<AudioObjectID>> {
        HOGGED_DEVICE.get_or_init(|| Mutex::new(None))
    }

    fn status_message(status: i32) -> String {
        format!("CoreAudio OSStatus {status}")
    }

    fn property_address(selector: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        }
    }

    fn output_device_ids() -> Result<Vec<AudioObjectID>, String> {
        let mut address = property_address(kAudioHardwarePropertyDevices);
        let mut data_size = 0u32;
        let status = unsafe {
            AudioObjectGetPropertyDataSize(
                kAudioObjectSystemObject as u32,
                NonNull::from(&mut address),
                0,
                null(),
                NonNull::from(&mut data_size),
            )
        };
        if status != 0 {
            return Err(status_message(status));
        }
        if data_size == 0 {
            return Err("CoreAudio reports no devices".into());
        }
        let mut ids = vec![0u32; data_size as usize / std::mem::size_of::<AudioObjectID>()];
        let status = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject as u32,
                NonNull::from(&mut address),
                0,
                null(),
                NonNull::from(&mut data_size),
                NonNull::new(ids.as_mut_ptr().cast::<c_void>()).unwrap(),
            )
        };
        if status != 0 {
            return Err(status_message(status));
        }
        Ok(ids)
    }

    fn has_output_stream(device: AudioObjectID) -> bool {
        let mut address = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyStreamConfiguration,
            mScope: kAudioObjectPropertyScopeOutput,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut data_size = 0u32;
        let status = unsafe {
            AudioObjectGetPropertyDataSize(
                device,
                NonNull::from(&mut address),
                0,
                null(),
                NonNull::from(&mut data_size),
            )
        };
        if status != 0 || data_size < std::mem::size_of::<u32>() as u32 {
            return false;
        }
        let word_count = data_size.div_ceil(std::mem::size_of::<u32>() as u32) as usize;
        let mut words = vec![0u32; word_count];
        let status = unsafe {
            AudioObjectGetPropertyData(
                device,
                NonNull::from(&mut address),
                0,
                null(),
                NonNull::from(&mut data_size),
                NonNull::new(words.as_mut_ptr().cast::<c_void>()).unwrap(),
            )
        };
        status == 0 && words.first().is_some_and(|count| *count > 0)
    }

    fn device_name(device: AudioObjectID) -> Option<String> {
        let mut address = property_address(kAudioObjectPropertyName);
        let mut value: *const CFString = std::ptr::null();
        let mut data_size = std::mem::size_of::<*const CFString>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                device,
                NonNull::from(&mut address),
                0,
                null(),
                NonNull::from(&mut data_size),
                NonNull::new((&mut value as *mut *const CFString).cast::<c_void>())?,
            )
        };
        if status != 0 {
            return None;
        }
        let ptr = NonNull::new(value.cast_mut())?;
        // CoreAudio returns the property as a borrowed CFStringRef. Retain it
        // for the short conversion so the Rust wrapper owns its lifetime.
        let string = unsafe { CFRetained::retain(ptr) };
        Some(string.to_string())
    }

    fn resolve_device(target: &str) -> Result<AudioObjectID, String> {
        let ids = output_device_ids()?;
        let mut exact = Vec::new();
        let mut fuzzy = Vec::new();
        for id in ids {
            if !has_output_stream(id) {
                continue;
            }
            let Some(name) = device_name(id) else {
                continue;
            };
            if name == target {
                exact.push(id);
                continue;
            }
            if name.to_lowercase().contains(&target.to_lowercase())
                || target.to_lowercase().contains(&name.to_lowercase())
            {
                fuzzy.push(id);
            }
        }
        let matches = if exact.is_empty() { &fuzzy } else { &exact };
        match matches.as_slice() {
            [device] => Ok(*device),
            [] => Err(format!(
                "could not match output device ‘{target}’ in CoreAudio"
            )),
            _ => Err(format!(
                "output device name ‘{target}’ is ambiguous in CoreAudio"
            )),
        }
    }

    fn physical_formats(
        device: AudioObjectID,
    ) -> Result<Vec<AudioStreamRangedDescription>, String> {
        let mut address = AudioObjectPropertyAddress {
            mSelector: kAudioStreamPropertyAvailablePhysicalFormats,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut data_size = 0u32;
        let status = unsafe {
            AudioObjectGetPropertyDataSize(
                device,
                NonNull::from(&mut address),
                0,
                null(),
                NonNull::from(&mut data_size),
            )
        };
        if status != 0 {
            return Err(status_message(status));
        }
        let count = data_size as usize / std::mem::size_of::<AudioStreamRangedDescription>();
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut formats = Vec::<AudioStreamRangedDescription>::with_capacity(count);
        let status = unsafe {
            AudioObjectGetPropertyData(
                device,
                NonNull::from(&mut address),
                0,
                null(),
                NonNull::from(&mut data_size),
                NonNull::new(formats.as_mut_ptr().cast::<c_void>()).unwrap(),
            )
        };
        if status != 0 {
            return Err(status_message(status));
        }
        let filled_bytes = data_size as usize;
        let entry_size = std::mem::size_of::<AudioStreamRangedDescription>();
        if filled_bytes > count * entry_size || filled_bytes % entry_size != 0 {
            return Err("CoreAudio returned an invalid physical-format list size".into());
        }
        unsafe {
            formats.set_len(filled_bytes / entry_size);
        }
        Ok(formats)
    }

    fn is_exact_integer_format(
        format: &AudioStreamBasicDescription,
        rate: u32,
        channels: u32,
        depth: u32,
    ) -> bool {
        format.mFormatID == kAudioFormatLinearPCM
            && format.mSampleRate == rate as f64
            && format.mChannelsPerFrame == channels
            && format.mBitsPerChannel == depth
            && format.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0
            && format.mFormatFlags & kAudioFormatFlagIsFloat == 0
            && format.mFormatFlags & kAudioFormatFlagIsPacked != 0
    }

    pub fn supports_integer_format(target: &str, rate: u32, channels: u32, depth: u32) -> bool {
        let Ok(device) = resolve_device(target) else {
            return false;
        };
        physical_formats(device).is_ok_and(|formats| {
            formats.into_iter().any(|entry| {
                let format = entry.mFormat;
                let range = entry.mSampleRateRange;
                format.mFormatID == kAudioFormatLinearPCM
                    && format.mChannelsPerFrame == channels
                    && format.mBitsPerChannel == depth
                    && format.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0
                    && format.mFormatFlags & kAudioFormatFlagIsFloat == 0
                    && format.mFormatFlags & kAudioFormatFlagIsPacked != 0
                    && (format.mSampleRate == rate as f64
                        || (range.mMinimum <= rate as f64 && rate as f64 <= range.mMaximum))
            })
        })
    }

    pub fn current_integer_format_matches(
        target: &str,
        rate: u32,
        channels: u32,
        depth: u32,
    ) -> bool {
        let Ok(device) = resolve_device(target) else {
            return false;
        };
        let mut address = AudioObjectPropertyAddress {
            mSelector: kAudioStreamPropertyPhysicalFormat,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut format = AudioStreamBasicDescription {
            mSampleRate: 0.0,
            mFormatID: 0,
            mFormatFlags: 0,
            mBytesPerPacket: 0,
            mFramesPerPacket: 0,
            mBytesPerFrame: 0,
            mChannelsPerFrame: 0,
            mBitsPerChannel: 0,
            mReserved: 0,
        };
        let mut data_size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                device,
                NonNull::from(&mut address),
                0,
                null(),
                NonNull::from(&mut data_size),
                NonNull::from(&mut format).cast::<c_void>(),
            )
        };
        status == 0 && is_exact_integer_format(&format, rate, channels, depth)
    }

    fn current_owner(device: AudioObjectID) -> Result<i32, String> {
        let mut address = property_address(kAudioDevicePropertyHogMode);
        let mut owner = -1i32;
        let mut data_size = std::mem::size_of::<i32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                device,
                NonNull::from(&mut address),
                0,
                null(),
                NonNull::from(&mut data_size),
                NonNull::from(&mut owner).cast(),
            )
        };
        if status != 0 {
            return Err(status_message(status));
        }
        Ok(owner)
    }

    fn set_hog(device: AudioObjectID, value: &mut i32) -> Result<(), String> {
        let mut address = property_address(kAudioDevicePropertyHogMode);
        let status = unsafe {
            AudioObjectSetPropertyData(
                device,
                NonNull::from(&mut address),
                0,
                null(),
                std::mem::size_of::<i32>() as u32,
                NonNull::from(value).cast::<c_void>(),
            )
        };
        if status != 0 {
            return Err(status_message(status));
        }
        Ok(())
    }

    pub fn acquire(target: &str) -> Result<(), String> {
        let device = resolve_device(target)?;
        if *hogged_device().lock().unwrap() == Some(device) {
            return Ok(());
        }
        release();
        let process = unsafe { libc::getpid() };
        let owner = current_owner(device)?;
        if owner == process {
            *hogged_device().lock().unwrap() = Some(device);
            return Ok(());
        }
        if owner != -1 {
            return Err("another process currently owns the output device".into());
        }
        let mut requested = process;
        set_hog(device, &mut requested)?;
        if requested != process || current_owner(device)? != process {
            return Err("CoreAudio did not grant exclusive access".into());
        }
        *hogged_device().lock().unwrap() = Some(device);
        Ok(())
    }

    pub fn release() {
        let Some(device) = hogged_device().lock().unwrap().take() else {
            return;
        };
        let process = unsafe { libc::getpid() };
        if current_owner(device).ok() == Some(process) {
            let mut release = -1;
            let _ = set_hog(device, &mut release);
        }
    }
}

pub(super) fn acquire_output_exclusive(device_name: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos_exclusive::acquire(device_name)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = device_name;
        Err("exclusive output is unavailable on this platform".into())
    }
}

pub(super) fn release_output_exclusive() {
    #[cfg(target_os = "macos")]
    macos_exclusive::release();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OutputFormat {
    pub(super) sample_rate: u32,
    pub(super) channels: usize,
    pub(super) pcm16: bool,
    pub(super) bit_perfect_depth: u32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OutputRange {
    pub(super) min_rate: u32,
    pub(super) max_rate: u32,
    pub(super) channels: usize,
    pub(super) pcm16: bool,
}

/// Chooses a stream format the selected device says it supports. A platform
/// rate is a preference, not proof the USB DAC or current route accepts it.
pub(super) fn choose_format(
    device: &cpal::Device,
    requested_rate: u32,
    requested_channels: u32,
    prefer_pcm16: bool,
) -> Result<OutputFormat, EngineError> {
    let default = device
        .default_output_config()
        .map_err(|e| EngineError::StreamInit(e.to_string()))?;
    let default_rate = default.sample_rate();
    let default_channels = default.channels() as usize;
    let prefer_pcm16 = prefer_pcm16 && !cfg!(target_os = "ios");

    let mut ranges: Vec<OutputRange> = device
        .supported_output_configs()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|range| {
            let pcm16 = match range.sample_format() {
                cpal::SampleFormat::I16 => true,
                cpal::SampleFormat::F32 => false,
                _ => return None,
            };
            if cfg!(target_os = "ios") && pcm16 {
                return None;
            }
            Some(OutputRange {
                min_rate: range.min_sample_rate(),
                max_rate: range.max_sample_rate(),
                channels: range.channels() as usize,
                pcm16,
            })
        })
        .collect();

    // Some backends expose only a default config. That config is a valid
    // fallback, but it does not authorize arbitrary requested rates/channels.
    if ranges.is_empty() {
        let pcm16 = match default.sample_format() {
            cpal::SampleFormat::I16 => true,
            cpal::SampleFormat::F32 => false,
            format => {
                return Err(EngineError::StreamInit(format!(
                    "unsupported default output sample format: {format:?}"
                )))
            }
        };
        if cfg!(target_os = "ios") && pcm16 {
            return Err(EngineError::StreamInit(
                "iOS output device does not expose float32 audio".into(),
            ));
        }
        ranges.push(OutputRange {
            min_rate: default_rate,
            max_rate: default_rate,
            channels: default_channels,
            pcm16,
        });
    }

    select_supported_format(
        &ranges,
        requested_rate,
        requested_channels as usize,
        default_rate,
        default_channels,
        prefer_pcm16,
    )
    .ok_or_else(|| {
        EngineError::StreamInit(
            "output device has no supported float32 or int16 configuration".into(),
        )
    })
}

pub(super) fn select_supported_format(
    ranges: &[OutputRange],
    requested_rate: u32,
    requested_channels: usize,
    default_rate: u32,
    default_channels: usize,
    prefer_pcm16: bool,
) -> Option<OutputFormat> {
    let desired_rate = if requested_rate > 0 {
        requested_rate
    } else {
        default_rate.max(1)
    };
    let desired_channels = if requested_channels > 0 {
        requested_channels
    } else {
        default_channels.max(1)
    };

    ranges
        .iter()
        .filter_map(|range| {
            if range.min_rate == 0 || range.min_rate > range.max_rate || range.channels == 0 {
                return None;
            }
            let rate = if (range.min_rate..=range.max_rate).contains(&desired_rate) {
                desired_rate
            } else if (range.min_rate..=range.max_rate).contains(&default_rate) {
                default_rate
            } else {
                desired_rate.clamp(range.min_rate, range.max_rate)
            };
            Some((
                (
                    range.channels == desired_channels,
                    rate == desired_rate,
                    range.channels == default_channels,
                    rate == default_rate,
                    range.pcm16 == prefer_pcm16,
                ),
                OutputFormat {
                    sample_rate: rate,
                    channels: range.channels,
                    pcm16: range.pcm16,
                    bit_perfect_depth: 0,
                },
            ))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, format)| format)
}

/// Builds an integer output configuration only when the route can open at
/// precisely the decoded source rate and stereo channel layout. CoreAudio's
/// macOS backend performs the physical stream-format/nominal-rate request when
/// this stream is opened; all other cases fall back to the ordinary mixer.
pub(super) fn choose_bitperfect_format(
    device: &cpal::Device,
    requested_rate: u32,
    requested_channels: u32,
    source_depth: u32,
) -> Result<Option<OutputFormat>, EngineError> {
    #[cfg(target_os = "macos")]
    {
        if !matches!(source_depth, 16 | 24)
            || requested_rate == 0
            || requested_channels != 2
            || !macos_exclusive::supports_integer_format(
                &device.to_string(),
                requested_rate,
                requested_channels,
                source_depth,
            )
        {
            return Ok(None);
        }
        Ok(Some(OutputFormat {
            sample_rate: requested_rate,
            channels: requested_channels as usize,
            pcm16: source_depth == 16,
            bit_perfect_depth: source_depth,
        }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (device, requested_rate, requested_channels, source_depth);
        Ok(None)
    }
}

pub(super) fn output_physical_format_matches(
    device_name: &str,
    rate: u32,
    channels: u32,
    depth: u32,
) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos_exclusive::current_integer_format_matches(device_name, rate, channels, depth)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (device_name, rate, channels, depth);
        false
    }
}

pub(super) fn open_output_stream(
    device: &cpal::Device,
    sample_rate: u32,
    channels: usize,
    pcm16: bool,
    bit_perfect_depth: u32,
    mut consumer: rtrb::Consumer<f32>,
    control: &OutputControl,
) -> Result<cpal::Stream, EngineError> {
    let config = cpal::StreamConfig {
        channels: channels as u16,
        sample_rate,
        buffer_size: cpal::BufferSize::Default,
    };
    let volume = control.volume_bits.clone();
    let sleep_gain = control.sleep_gain_bits.clone();
    let buffered = control.buffered.clone();
    let callback_underruns = control.callback_underruns.clone();
    let output_peak = control.output_peak.clone();
    let flush_ring = control.flush_ring.clone();
    let bail_flush = control.bail_flush.clone();
    let paused = control.output_paused.clone();
    let holds = control.output_holds.clone();
    let err_ctrl = control.clone();
    if bit_perfect_depth == 16 {
        let mut scratch = vec![0f32; (sample_rate as usize * channels).max(8192)];
        return device
            .build_output_stream(
                config,
                move |data: &mut [i16], _| {
                    if holds.load(Ordering::Acquire) > 0 {
                        data.fill(Default::default());
                        return;
                    }
                    if scratch.len() < data.len() {
                        data.fill(Default::default());
                        callback_underruns.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    render_bitperfect(
                        &mut consumer,
                        &buffered,
                        &callback_underruns,
                        &output_peak,
                        &flush_ring,
                        &bail_flush,
                        &paused,
                        &mut scratch[..data.len()],
                    );
                    for (out, sample) in data.iter_mut().zip(&scratch) {
                        *out = exact_f32_to_i16(
                            *sample * f32::from_bits(sleep_gain.load(Ordering::Relaxed)),
                        );
                    }
                },
                move |err| err_ctrl.on_stream_error(err),
                None,
            )
            .map_err(|e| EngineError::StreamInit(e.to_string()));
    }
    if bit_perfect_depth == 24 {
        let mut scratch = vec![0f32; (sample_rate as usize * channels).max(8192)];
        return device
            .build_output_stream(
                config,
                move |data: &mut [cpal::I24], _| {
                    if holds.load(Ordering::Acquire) > 0 {
                        data.fill(Default::default());
                        return;
                    }
                    if scratch.len() < data.len() {
                        data.fill(Default::default());
                        callback_underruns.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    render_bitperfect(
                        &mut consumer,
                        &buffered,
                        &callback_underruns,
                        &output_peak,
                        &flush_ring,
                        &bail_flush,
                        &paused,
                        &mut scratch[..data.len()],
                    );
                    for (out, sample) in data.iter_mut().zip(&scratch) {
                        *out = cpal::I24::new_unchecked(exact_f32_to_i24(
                            *sample * f32::from_bits(sleep_gain.load(Ordering::Relaxed)),
                        ));
                    }
                },
                move |err| err_ctrl.on_stream_error(err),
                None,
            )
            .map_err(|e| EngineError::StreamInit(e.to_string()));
    }
    // PCM_16 opens the unit as int16: the ring stays f32 throughout (mix, EQ
    // and volume all run in float, exactly as upstream's processors run before
    // the sink's conversion) and quantization happens once, at the boundary.
    // FLOAT_32 is the passthrough — the mix format is the wire format.
    //
    // On iOS CoreAudio, RemoteIO requires 32-bit float streams; CPAL's i16 path
    // fails to render to hardware or produces silence/choppy audio. Force f32 on iOS.
    let pcm16 = if cfg!(target_os = "ios") {
        false
    } else {
        pcm16
    };
    if pcm16 {
        let mut scratch = vec![0.0f32; (sample_rate as usize * channels).max(8192)];
        let mut bail_ramp = BailRamp {
            app_gain: 1.0,
            recovery_gain: 1.0,
            last_frame: [0.0; 2],
            primed: false,
            prebuffer: true,
            waited_frames: 0,
            buffer: Vec::with_capacity((sample_rate as usize * 120 / 1000) * 2),
            cursor: 0,
            total_frames: 0,
        };
        device
            .build_output_stream(
                config,
                move |data: &mut [i16], _| {
                    if holds.load(Ordering::Acquire) > 0 {
                        data.fill(Default::default());
                        return;
                    }
                    if scratch.len() < data.len() {
                        data.fill(Default::default());
                        callback_underruns.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    let frames = &mut scratch[..data.len()];
                    render_f32(
                        &mut consumer,
                        &volume,
                        &buffered,
                        &callback_underruns,
                        &output_peak,
                        &flush_ring,
                        &bail_flush,
                        &paused,
                        &mut bail_ramp,
                        sample_rate,
                        channels,
                        frames,
                    );
                    for (out, s) in data.iter_mut().zip(frames.iter()) {
                        *out = clamp16_from_float(*s);
                    }
                },
                move |err| err_ctrl.on_stream_error(err),
                None,
            )
            .map_err(|e| EngineError::StreamInit(e.to_string()))
    } else {
        let mut bail_ramp = BailRamp {
            app_gain: 1.0,
            recovery_gain: 1.0,
            last_frame: [0.0; 2],
            primed: false,
            prebuffer: true,
            waited_frames: 0,
            buffer: Vec::with_capacity((sample_rate as usize * 120 / 1000) * 2),
            cursor: 0,
            total_frames: 0,
        };
        device
            .build_output_stream(
                config,
                move |data: &mut [f32], _| {
                    if holds.load(Ordering::Acquire) > 0 {
                        data.fill(Default::default());
                        return;
                    }
                    render_f32(
                        &mut consumer,
                        &volume,
                        &buffered,
                        &callback_underruns,
                        &output_peak,
                        &flush_ring,
                        &bail_flush,
                        &paused,
                        &mut bail_ramp,
                        sample_rate,
                        channels,
                        data,
                    );
                },
                move |err| err_ctrl.on_stream_error(err),
                None,
            )
            .map_err(|e| EngineError::StreamInit(e.to_string()))
    }
}
