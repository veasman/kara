//! Video wallpaper decode via GStreamer.
//!
//! Uses `playbin3` wrapped around a custom video-sink bin
//! (`videoconvert ! videoscale ! video/x-raw,format=BGRA ! appsink`).
//! Each decoded sample lands in a shared `Mutex<Option>` slot; the
//! compositor's wallpaper tick drains that slot once per frame and
//! re-uploads to the GPU.
//!
//! **Seamless looping** is done via playbin3's `about-to-finish`
//! signal, not a flush-seek on EOS. `about-to-finish` fires on the
//! streaming thread a little before the current URI runs out, and
//! re-setting the `uri` property during the handler causes playbin3
//! to start decoding the replacement source in parallel and splice
//! it onto the downstream pipeline without ever flushing. This is
//! the canonical GStreamer gapless-playback pattern — same one
//! rhythmbox / Clementine / etc. use for seamless audio — and it
//! works for video too because playbin3 chains sources through
//! `urisourcebin`.
//!
//! Audio is suppressed via `flags=video` so audio tracks in the
//! source file don't try to spin up a PulseAudio/PipeWire sink.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// A single decoded video frame. The serial counter increments on
/// every new sample so the compositor can detect "is this a new
/// frame or the same one we already uploaded?" cheaply.
pub struct VideoFrame {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub serial: u64,
}

/// Shared slot written by the streaming thread, read by the main
/// loop. Holds the most recent frame only — old frames are dropped
/// on the floor if the compositor falls behind, which is exactly
/// the right behavior for a background wallpaper.
pub type FrameSlot = Arc<Mutex<Option<VideoFrame>>>;

/// Running GStreamer pipeline for one video wallpaper. Drops
/// automatically tear down the pipeline via the bus watch remove
/// + state set to Null.
pub struct VideoStream {
    pipeline: gst::Pipeline,
    latest: FrameSlot,
    /// Cached (width, height) from the first decoded sample so
    /// callers that want dimensions before the first tick don't
    /// have to wait.
    dimensions: Arc<Mutex<Option<(u32, u32)>>>,
    /// Signals the bus-poll thread to exit. Set in `Drop` before
    /// tearing down the pipeline.
    stop_flag: Arc<AtomicBool>,
    /// Join handle for the bus-poll thread so Drop can wait for
    /// it to exit cleanly before the pipeline is set to Null.
    bus_thread: Option<std::thread::JoinHandle<()>>,
}

impl VideoStream {
    /// Build a decode pipeline for `path` and start it playing.
    /// Blocks until GStreamer reports the pipeline is in `Playing`
    /// state AND the first sample has been pulled — this gives
    /// callers an initial frame + real dimensions without a
    /// visible black flash on theme switch.
    ///
    /// Returns None if the file can't be decoded, GStreamer isn't
    /// initialized, or the first sample doesn't arrive within a
    /// reasonable timeout.
    pub fn load(path: &std::path::Path) -> Option<Self> {
        if gst::init().is_err() {
            tracing::error!("gstreamer init failed — video wallpapers unavailable");
            return None;
        }

        let uri = match gst::glib::filename_to_uri(path, None) {
            Ok(u) => u.to_string(),
            Err(e) => {
                tracing::error!("failed to build gst uri for '{}': {e}", path.display());
                return None;
            }
        };

        // Build the video-sink bin: videoconvert + videoscale
        // + caps filter forcing BGRA + an appsink the compositor
        // pulls decoded frames from. BGRA maps to
        // `Fourcc::Argb8888` on little-endian so the buffer can
        // go straight to `TextureBuffer::from_memory`.
        let sink_bin_desc = "videoconvert ! videoscale \
            ! video/x-raw,format=BGRA \
            ! appsink name=sink drop=true max-buffers=2 sync=true";
        let sink_bin = match gst::parse::bin_from_description(sink_bin_desc, true) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("gst sink bin build failed: {e}");
                return None;
            }
        };
        let sink_element = sink_bin
            .by_name("sink")?
            .downcast::<gst_app::AppSink>()
            .ok()?;

        // playbin3 wraps source + demux + decode + sink into one
        // element and, crucially, supports seamless gapless chaining
        // via the `about-to-finish` signal. We hand it the sink bin
        // above as its video-sink and suppress audio entirely with
        // `flags=video` (bit 0 of GstPlayFlags).
        let playbin = match gst::ElementFactory::make("playbin3")
            .name("kara-video")
            .property("uri", &uri)
            .property("video-sink", &sink_bin.upcast_ref::<gst::Element>())
            .build()
        {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("failed to create playbin3: {e}");
                return None;
            }
        };
        // GstPlayFlags::VIDEO = 0x1. Setting just this bit tells
        // playbin to not even try to link the audio stream — no
        // fakesink needed, no PipeWire wakeup.
        playbin.set_property_from_str("flags", "video");

        // Wrap playbin in a Pipeline so the rest of the module
        // (bus, state transitions, Drop teardown) keeps working.
        // playbin3 IS a Pipeline subclass already — downcast.
        let pipeline = match playbin.clone().downcast::<gst::Pipeline>() {
            Ok(p) => p,
            Err(_) => {
                tracing::error!("playbin3 is not a Pipeline — unexpected");
                return None;
            }
        };

        // Seamless loop via `about-to-finish`. playbin3 fires this
        // signal on the streaming thread a little before the
        // current URI finishes; setting `uri` during the handler
        // tells playbin3 to start pulling the next source in
        // parallel and splice it in without ever flushing the
        // downstream sink bin. Same URI = perfect loop.
        let uri_loop = uri.clone();
        playbin.connect("about-to-finish", false, move |values| {
            if let Ok(pb) = values[0].get::<gst::Element>() {
                pb.set_property("uri", &uri_loop);
            }
            None
        });

        let sink = sink_element;

        let latest: FrameSlot = Arc::new(Mutex::new(None));
        let dimensions: Arc<Mutex<Option<(u32, u32)>>> = Arc::new(Mutex::new(None));
        let serial = Arc::new(Mutex::new(0u64));

        let latest_cb = latest.clone();
        let dims_cb = dimensions.clone();
        let serial_cb = serial.clone();

        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    let sample = match s.pull_sample() {
                        Ok(s) => s,
                        Err(_) => return Err(gst::FlowError::Eos),
                    };
                    let buffer = match sample.buffer() {
                        Some(b) => b,
                        None => return Ok(gst::FlowSuccess::Ok),
                    };
                    let caps = match sample.caps() {
                        Some(c) => c,
                        None => return Ok(gst::FlowSuccess::Ok),
                    };
                    let info = match gstreamer_video::VideoInfo::from_caps(caps) {
                        Ok(i) => i,
                        Err(_) => return Ok(gst::FlowSuccess::Ok),
                    };
                    let w = info.width();
                    let h = info.height();
                    let map = match buffer.map_readable() {
                        Ok(m) => m,
                        Err(_) => return Ok(gst::FlowSuccess::Ok),
                    };

                    let mut next_serial = serial_cb.lock().unwrap();
                    *next_serial = next_serial.wrapping_add(1);
                    let sn = *next_serial;
                    drop(next_serial);

                    *dims_cb.lock().unwrap() = Some((w, h));
                    *latest_cb.lock().unwrap() = Some(VideoFrame {
                        bgra: map.as_slice().to_vec(),
                        width: w,
                        height: h,
                        serial: sn,
                    });

                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        // Dedicated bus-poll thread for error logging. kara-gate
        // runs on calloop, not a glib MainContext, so `bus.add_watch`
        // never dispatches — a worker thread with `timed_pop_filtered`
        // is the canonical pattern for non-glib hosts. Looping is
        // handled by `about-to-finish` above, so EOS is no longer
        // expected during normal playback; if it does arrive we log
        // it as an unexpected end-of-stream.
        let bus = pipeline.bus()?;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_thread = stop_flag.clone();
        let path_bus = path.display().to_string();
        let bus_thread = std::thread::spawn(move || {
            use gst::MessageView;
            while !stop_flag_thread.load(Ordering::Relaxed) {
                let msg = bus.timed_pop_filtered(
                    gst::ClockTime::from_mseconds(100),
                    &[gst::MessageType::Eos, gst::MessageType::Error],
                );
                let msg = match msg {
                    Some(m) => m,
                    None => continue,
                };
                match msg.view() {
                    MessageView::Eos(_) => {
                        tracing::warn!(
                            "video wallpaper '{path_bus}' reached EOS — \
                             about-to-finish re-uri probably failed"
                        );
                    }
                    MessageView::Error(err) => {
                        tracing::error!(
                            "video wallpaper '{path_bus}' decode error: {} ({:?})",
                            err.error(),
                            err.debug()
                        );
                    }
                    _ => {}
                }
            }
        });

        if pipeline.set_state(gst::State::Playing).is_err() {
            tracing::error!("failed to start video pipeline for '{}'", path.display());
            stop_flag.store(true, Ordering::Relaxed);
            let _ = bus_thread.join();
            return None;
        }

        // Wait briefly for the first sample so the compositor has
        // real dimensions + pixels on the first render. We budget
        // 1 second; most files deliver the first frame in < 100ms.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1000);
        loop {
            if latest.lock().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() > deadline {
                tracing::warn!(
                    "video wallpaper '{}' did not produce a frame within 1s; \
                     proceeding anyway",
                    path.display()
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        Some(Self {
            pipeline,
            latest,
            dimensions,
            stop_flag,
            bus_thread: Some(bus_thread),
        })
    }

    /// Peek the cached dimensions. Returns (1, 1) if no frame has
    /// arrived yet so the aspect-crop math doesn't divide by zero.
    pub fn dimensions(&self) -> (u32, u32) {
        self.dimensions.lock().unwrap().unwrap_or((1, 1))
    }

    /// Take the latest decoded frame out of the shared slot. The
    /// caller uploads it to the GPU; if no new frame has arrived
    /// since the last call this returns None and the caller keeps
    /// showing the previous texture.
    pub fn take_latest(&self) -> Option<VideoFrame> {
        self.latest.lock().unwrap().take()
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        // Signal the bus thread to stop, then tear the pipeline
        // down. Joining the thread before Null state ensures no
        // in-flight seek races with the state transition.
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.bus_thread.take() {
            let _ = handle.join();
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
