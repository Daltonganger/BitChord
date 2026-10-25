//! The host's callback, and the bridge that turns mixer events into calls on it.

use crate::mixer::{EngineEvents, TrackInfo};
use crate::records::{PlaybackState, TrackEndReason, TrackInfoRec};
use std::sync::{Arc, Mutex};

/// Engine events delivered from the mixer thread — implementors must hop to
/// the main thread as needed.
#[uniffi::export(callback_interface)]
pub trait EngineCallback: Send + Sync {
    fn on_state_changed(&self, state: PlaybackState);
    fn on_track_ended(&self, reason: TrackEndReason, source: String);
    fn on_error(&self, message: String);
    /// The incoming track became audible — current-track metadata flips here.
    fn on_handoff(&self, info: TrackInfoRec);
    fn on_duration_changed(&self, seconds: f64);
}

pub(super) struct CallbackHolder(pub(super) Arc<Box<dyn EngineCallback>>);
unsafe impl Send for CallbackHolder {}
unsafe impl Sync for CallbackHolder {}

/// Fan-out cell: the mixer holds this and forwards events to whatever
/// callback is registered (registration may happen before or after start).
#[derive(Default)]
pub(super) struct SharedEvents {
    pub(super) inner: Mutex<Option<CallbackHolder>>,
}

impl EngineEvents for SharedEvents {
    fn state_changed(&self, state: PlaybackState) {
        if let Some(holder) = self.inner.lock().unwrap().as_ref() {
            holder.0.on_state_changed(state);
        }
    }
    fn track_ended(&self, reason: TrackEndReason, source: String) {
        if let Some(holder) = self.inner.lock().unwrap().as_ref() {
            holder.0.on_track_ended(reason, source);
        }
    }
    fn error(&self, message: String) {
        if let Some(holder) = self.inner.lock().unwrap().as_ref() {
            holder.0.on_error(message);
        }
    }
    fn handoff(&self, info: TrackInfo) {
        if let Some(holder) = self.inner.lock().unwrap().as_ref() {
            holder.0.on_handoff(TrackInfoRec {
                title: info.title,
                artist: info.artist,
                source: info.source,
                duration_seconds: info.duration_seconds,
                codec: info.codec,
                sample_rate: info.sample_rate,
                bit_depth: info.bit_depth,
                channels: info.channels,
                kbps: info.kbps,
            });
        }
    }
    fn duration_changed(&self, seconds: f64) {
        if let Some(holder) = self.inner.lock().unwrap().as_ref() {
            holder.0.on_duration_changed(seconds);
        }
    }
}
