//! An HTTP media source: ranged fetches into a shared buffer, read and
//! seeked like a file.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use symphonia::core::io::MediaSource;

pub(super) struct StreamBuffer {
    pub(super) data: Vec<u8>,
    /// File offset of `data[0]`.
    pub(super) start: u64,
    pub(super) read_pos: u64,
    pub(super) total_len: Option<u64>,
    pub(super) fetch_dead: bool,
    pub(super) fetch_error: Option<String>,
}

impl StreamBuffer {
    pub(super) fn end(&self) -> u64 {
        self.start + self.data.len() as u64
    }

    /// Bytes already fetched that sit at or after `read_pos`. Zero when the
    /// cursor has been left behind `start` (a seek restarted the window
    /// without moving the cursor — the old `loaded()` treated that as a
    /// huge positive length and the read path underflowed).
    pub(super) fn loaded(&self) -> u64 {
        if self.read_pos < self.start {
            0
        } else {
            self.end().saturating_sub(self.read_pos)
        }
    }

    /// Index of `read_pos` inside `data`, if it still lands in the window.
    pub(super) fn cursor(&self) -> Option<usize> {
        let rel = self.read_pos.checked_sub(self.start)?;
        let rel = usize::try_from(rel).ok()?;
        (rel <= self.data.len()).then_some(rel)
    }
}

pub(super) fn lock_buf(mutex: &Mutex<StreamBuffer>) -> std::sync::MutexGuard<'_, StreamBuffer> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn lock_handle(
    mutex: &Mutex<Option<JoinHandle<()>>>,
) -> std::sync::MutexGuard<'_, Option<JoinHandle<()>>> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) struct FetchState {
    pub(super) buffer: Mutex<StreamBuffer>,
    pub(super) cond: Condvar,
    /// Generation counter — incremented on every range restart so a stale
    /// fetcher from a superseded request cannot keep writing.
    pub(super) generation: AtomicU64,
    pub(super) kill: AtomicBool,
}

/// `MediaSource` backed by a background ranged HTTP fetcher.
pub(super) struct HttpMediaSource {
    pub(super) url: String,
    pub(super) headers: HashMap<String, String>,
    pub(super) state: Arc<FetchState>,
    pub(super) fetcher: Mutex<Option<JoinHandle<()>>>,
}

impl HttpMediaSource {
    pub(super) fn open(url: &str, headers: &HashMap<String, String>) -> Result<Self, String> {
        let state = Arc::new(FetchState {
            buffer: Mutex::new(StreamBuffer {
                data: Vec::with_capacity(1 << 20),
                start: 0,
                read_pos: 0,
                total_len: None,
                fetch_dead: false,
                fetch_error: None,
            }),
            cond: Condvar::new(),
            generation: AtomicU64::new(0),
            kill: AtomicBool::new(false),
        });
        let source = Self {
            url: url.to_string(),
            headers: headers.clone(),
            state: state.clone(),
            fetcher: Mutex::new(None),
        };
        source.spawn_fetch(0);
        Ok(source)
    }

    pub(super) fn spawn_fetch(&self, from: u64) {
        let generation = self.state.generation.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut buf = lock_buf(&self.state.buffer);
            buf.fetch_dead = false;
            buf.fetch_error = None;
            // Keep an overlapping tail when the new origin still sits inside
            // the current window; otherwise drop it — appending onto stale
            // bytes would desync `start` from the HTTP range.
            if from >= buf.start && from <= buf.end() {
                let cut = (from - buf.start) as usize;
                let cut = cut.min(buf.data.len());
                buf.data.drain(..cut);
            } else {
                buf.data.clear();
            }
            buf.start = from;
            buf.read_pos = from;
        }
        self.state.kill.store(false, Ordering::SeqCst);
        let url = self.url.clone();
        let headers = self.headers.clone();
        let state = self.state.clone();
        let handle = std::thread::Builder::new()
            .name("native-core-http".into())
            .spawn(move || fetch_loop(url, headers, state, from, generation))
            .expect("spawn fetch thread");
        *lock_handle(&self.fetcher) = Some(handle);
    }
}

/// googlevideo throttles an open-ended `Range: bytes=N-` down to playback
/// speed (~15 kB/s). Bounded 1 MiB ranges arrive at line rate — same as
/// upstream `ChunkedDataSource` (2 MiB there; 1 MiB here because some
/// networks 403 a first range larger than that).
pub(super) const HTTP_CHUNK: u64 = 1 * 1024 * 1024;

pub(super) fn fetch_loop(
    url: String,
    headers: HashMap<String, String>,
    state: Arc<FetchState>,
    from: u64,
    generation: u64,
) {
    let result = (|| -> Result<(), String> {
        let mut pos = from;
        loop {
            if state.kill.load(Ordering::SeqCst)
                || state.generation.load(Ordering::SeqCst) != generation
            {
                return Ok(());
            }
            let known_total = lock_buf(&state.buffer).total_len;
            if known_total.is_some_and(|t| pos >= t) {
                return Ok(());
            }
            let end = known_total
                .map(|t| (pos + HTTP_CHUNK - 1).min(t.saturating_sub(1)))
                .unwrap_or(pos + HTTP_CHUNK - 1);
            let range = format!("bytes={pos}-{end}");
            let mut req = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(20))
                .set("Range", &range);
            for (key, value) in &headers {
                req = req.set(key, value);
            }
            let response = match req.call() {
                Ok(r) => r,
                // Past EOF (MP4 probe seeking to moov, or a CDN that does not
                // honour a range past Content-Length). Same as a clean end.
                Err(ureq::Error::Status(416, _)) => return Ok(()),
                Err(e) => return Err(format!("GET: {e}")),
            };

            {
                let mut buf = lock_buf(&state.buffer);
                if buf.total_len.is_none() {
                    if let Some(range) = response.header("content-range") {
                        if let Some(total) =
                            range.rsplit('/').next().and_then(|t| t.parse::<u64>().ok())
                        {
                            if total > 0 {
                                buf.total_len = Some(total);
                            }
                        }
                    } else if let Some(len) = response
                        .header("content-length")
                        .and_then(|l| l.parse::<u64>().ok())
                    {
                        buf.total_len = Some(pos + len);
                    }
                }
            }

            let mut reader = response.into_reader();
            let mut chunk = [0u8; 64 * 1024];
            let mut got: u64 = 0;
            loop {
                if state.kill.load(Ordering::SeqCst)
                    || state.generation.load(Ordering::SeqCst) != generation
                {
                    return Ok(());
                }
                let n = reader.read(&mut chunk).map_err(|e| format!("read: {e}"))?;
                if n == 0 {
                    break;
                }
                got += n as u64;
                {
                    let mut buf = lock_buf(&state.buffer);
                    buf.data.extend_from_slice(&chunk[..n]);
                    state.cond.notify_all();
                }
                loop {
                    if state.kill.load(Ordering::SeqCst)
                        || state.generation.load(Ordering::SeqCst) != generation
                    {
                        return Ok(());
                    }
                    let buffered = {
                        let buf = lock_buf(&state.buffer);
                        buf.loaded() as i64
                    };
                    if buffered < 32 * 1024 * 1024 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            if got == 0 {
                return Ok(());
            }
            pos += got;
            let total = lock_buf(&state.buffer).total_len;
            if got < HTTP_CHUNK || total.is_some_and(|t| pos >= t) {
                return Ok(());
            }
        }
    })();

    let mut buf = lock_buf(&state.buffer);
    if let Err(e) = result {
        log::error!("HttpMediaSource fetch failed: {e}");
        buf.fetch_error = Some(e);
    }
    buf.fetch_dead = true;
    state.cond.notify_all();
}

impl Read for HttpMediaSource {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut buf = lock_buf(&self.state.buffer);
        loop {
            if let Some(err) = &buf.fetch_error {
                if buf.loaded() == 0 {
                    return Err(std::io::Error::other(err.clone()));
                }
                // Serve what remains before surfacing the failure.
            }
            if let Some(offset) = buf.cursor() {
                let available = buf.data.len().saturating_sub(offset);
                if available > 0 {
                    let take = out.len().min(available);
                    out[..take].copy_from_slice(&buf.data[offset..offset + take]);
                    buf.read_pos += take as u64;
                    // Drop consumed bytes behind a 4 MB window so the buffer does
                    // not grow without bound on long tracks.
                    const KEEP_BEHIND: usize = 4 * 1024 * 1024;
                    let consumed = buf
                        .read_pos
                        .saturating_sub(buf.start)
                        .min(buf.data.len() as u64) as usize;
                    if consumed > KEEP_BEHIND {
                        let cut = consumed - KEEP_BEHIND;
                        buf.data.drain(..cut);
                        buf.start += cut as u64;
                    }
                    return Ok(take);
                }
            }
            if buf.fetch_dead {
                return Ok(0); // clean EOF
            }
            buf = self.state.cond.wait(buf).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Seek for HttpMediaSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let total = lock_buf(&self.state.buffer).total_len;
        let mut buf = lock_buf(&self.state.buffer);
        let target = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(delta) => buf.read_pos as i64 + delta,
            SeekFrom::End(delta) => {
                let end = total
                    .ok_or_else(|| std::io::Error::other("seek from end with unknown length"))?
                    as i64;
                end + delta
            }
        };
        if target < 0 {
            return Err(std::io::Error::other("negative seek"));
        }
        let target = target as u64;

        // MP4 probe commonly seeks to the moov atom past the first range, or
        // back into bytes already dropped by KEEP_BEHIND. Either side of the
        // window needs a fresh ranged fetch — and `read_pos` must move with
        // `start` or the next read underflows.
        if target < buf.start || target > buf.end() {
            drop(buf);
            self.state.kill.store(true, Ordering::SeqCst);
            if let Some(handle) = lock_handle(&self.fetcher).take() {
                let _ = handle.join();
            }
            self.spawn_fetch(target);
        } else {
            buf.read_pos = target;
        }
        Ok(target)
    }
}

impl MediaSource for HttpMediaSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        lock_buf(&self.state.buffer).total_len
    }
}

impl Drop for HttpMediaSource {
    fn drop(&mut self) {
        self.state.kill.store(true, Ordering::SeqCst);
        self.state.cond.notify_all();
        if let Some(handle) = lock_handle(&self.fetcher).take() {
            let _ = handle.join();
        }
    }
}
