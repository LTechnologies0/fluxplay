//! Takes FFmpeg frames off the iced thread. The UI only swaps the latest one.
//!
//! Before the app's GPU stage exists, frames are RGBA scaled to the stage
//! size. Once it is installed they are native YUV at decoded size, and CUDA
//! frames stay in VRAM when the stage can export buffers to the decoder.

use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tracing::{info, warn};

use crate::ffmpeg_ffi::LibFfmpeg;
use crate::video_frame::{
    video_stage, video_stage_ready, ColorMatrix, FrameData, GpuFrame, PixelLayout, SlotRelease,
    VideoFrame,
};

/// Zero-copy slots: last published, UI copies in flight, one being written.
const GPU_SLOTS: usize = 4;

struct Slot {
    frame: VideoFrame,
    gen: u32,
}

struct GpuSet {
    slots: Arc<dyn Any + Send + Sync>,
    release: Arc<SlotRelease>,
}

pub struct FfmpegPump {
    stop: Arc<AtomicBool>,
    want_w: Arc<AtomicU32>,
    want_h: Arc<AtomicU32>,
    latest: Arc<Mutex<Option<Slot>>>,
    recycled: Arc<Mutex<Option<Vec<u8>>>>,
    produced: Arc<AtomicU32>,
    consumed: Arc<AtomicU32>,
    join: Option<JoinHandle<()>>,
}

impl FfmpegPump {
    pub fn start(ff: Arc<LibFfmpeg>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let want_w = Arc::new(AtomicU32::new(1280));
        let want_h = Arc::new(AtomicU32::new(720));
        let latest = Arc::new(Mutex::new(None));
        let recycled = Arc::new(Mutex::new(None));
        let produced = Arc::new(AtomicU32::new(0));
        let consumed = Arc::new(AtomicU32::new(0));

        let stop_t = Arc::clone(&stop);
        let want_w_t = Arc::clone(&want_w);
        let want_h_t = Arc::clone(&want_h);
        let latest_t = Arc::clone(&latest);
        let recycled_t = Arc::clone(&recycled);
        let produced_t = Arc::clone(&produced);

        let join = thread::Builder::new()
            .name("fp-ffmpeg-pump".into())
            .spawn(move || {
                let mut gen = 0u32;
                let mut yuv = false;
                let mut gpu: Option<GpuSet> = None;
                let mut refused: Option<(u32, u32, u32)> = None;
                while !stop_t.load(Ordering::Acquire) {
                    if !yuv && video_stage_ready() {
                        ff.set_yuv_output(true);
                        yuv = true;
                        info!("ffmpeg pump: native YUV frames for the GPU stage");
                    }
                    if let Some(set) = &gpu {
                        let mut mask = set.release.take();
                        while mask != 0 {
                            let slot = mask.trailing_zeros();
                            ff.gpu_release(slot);
                            mask &= mask - 1;
                        }
                    }
                    if yuv {
                        if let Some(need) = ff.gpu_need() {
                            if refused != Some(need) {
                                match attach_slots(&ff, need) {
                                    Some(set) => gpu = Some(set),
                                    None => refused = Some(need),
                                }
                            }
                        }
                    }
                    if !ff.has_frame() {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    /* Always take the published frame so present can clear
                     * frame_ready on the film clock. Holding until the UI
                     * finished GPU upload made present overwrite/drop and
                     * hitch. The UI may skip intermediates under load. */
                    let frame = if yuv {
                        let mut buf = recycled_t
                            .lock()
                            .ok()
                            .and_then(|mut r| r.take())
                            .unwrap_or_default();
                        let give_back = |buf: Vec<u8>| {
                            if let Ok(mut r) = recycled_t.lock() {
                                r.get_or_insert(buf);
                            }
                        };
                        match ff.pull_frame(&mut buf) {
                            Some(p) => {
                                let data = match (p.gpu_slot, &gpu) {
                                    (Some(slot), Some(set)) => {
                                        give_back(buf);
                                        FrameData::Gpu(GpuFrame::new(
                                            Arc::clone(&set.slots),
                                            slot,
                                            Arc::clone(&set.release),
                                        ))
                                    }
                                    (Some(slot), None) => {
                                        give_back(buf);
                                        ff.gpu_release(slot);
                                        continue;
                                    }
                                    (None, _) => FrameData::Cpu(buf),
                                };
                                Some(VideoFrame {
                                    layout: p.layout,
                                    width: p.width,
                                    height: p.height,
                                    pitch: p.pitch,
                                    sar: p.sar,
                                    matrix: p.matrix,
                                    full_range: p.full_range,
                                    data,
                                })
                            }
                            None => {
                                give_back(buf);
                                None
                            }
                        }
                    } else {
                        let w = want_w_t.load(Ordering::Relaxed);
                        let h = want_h_t.load(Ordering::Relaxed);
                        ff.pull_rgba(w, h).map(|(fw, fh, pixels)| VideoFrame {
                            layout: PixelLayout::Rgba,
                            width: fw,
                            height: fh,
                            pitch: fw * 4,
                            sar: (1, 1),
                            matrix: ColorMatrix::Bt709,
                            full_range: true,
                            data: FrameData::Cpu(pixels),
                        })
                    };
                    match frame {
                        Some(frame) => {
                            gen = gen.wrapping_add(1);
                            produced_t.store(gen, Ordering::Release);
                            if let Ok(mut slot) = latest_t.lock() {
                                slot.replace(Slot { frame, gen });
                            }
                        }
                        None => thread::sleep(Duration::from_millis(1)),
                    }
                }
            })
            .ok();

        Self {
            stop,
            want_w,
            want_h,
            latest,
            recycled,
            produced,
            consumed,
            join,
        }
    }

    pub fn set_target(&self, w: u32, h: u32) {
        self.want_w.store((w.clamp(2, 3840) & !1).max(2), Ordering::Relaxed);
        self.want_h.store((h.clamp(2, 2160) & !1).max(2), Ordering::Relaxed);
    }

    pub fn needs_redraw(&self) -> bool {
        self.produced.load(Ordering::Acquire) != self.consumed.load(Ordering::Acquire)
    }

    pub fn take_frame(&self) -> Option<VideoFrame> {
        let slot = self.latest.lock().ok()?.take()?;
        self.consumed.store(slot.gen, Ordering::Release);
        Some(slot.frame)
    }

    /// Give a CPU frame buffer back for the next pull.
    pub fn recycle(&self, buf: Vec<u8>) {
        if let Ok(mut r) = self.recycled.lock() {
            if r.as_ref().is_none_or(|old| old.capacity() < buf.capacity()) {
                *r = Some(buf);
            }
        }
    }
}

#[cfg(unix)]
fn attach_slots(ff: &LibFfmpeg, (w, h, bpc): (u32, u32, u32)) -> Option<GpuSet> {
    let stage = video_stage()?;
    let layout = if bpc == 2 { PixelLayout::P010 } else { PixelLayout::Nv12 };
    let Some(exported) = stage.export_slots(GPU_SLOTS, layout, w, h) else {
        info!("GPU stage cannot export buffers — decoded frames go through RAM");
        return None;
    };
    let slots = Arc::clone(&exported.slots);
    if !ff.gpu_attach(exported, w, h, bpc) {
        warn!("CUDA refused the GPU stage buffers — decoded frames go through RAM");
        return None;
    }
    Some(GpuSet {
        slots,
        release: Arc::new(SlotRelease::default()),
    })
}

#[cfg(not(unix))]
fn attach_slots(_ff: &LibFfmpeg, _need: (u32, u32, u32)) -> Option<GpuSet> {
    let _ = video_stage();
    None
}

impl Drop for FfmpegPump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
