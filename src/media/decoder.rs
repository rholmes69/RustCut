//! Streaming video decoder: one FFmpeg process producing raw RGBA frames.

use crate::media::ffmpeg::{command, ffmpeg_bin};
use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc::{sync_channel, Receiver};
use std::thread;

pub struct Frame {
    /// Timeline time this frame belongs to.
    pub time: f64,
    pub data: Vec<u8>,
}

pub struct VideoStream {
    child: Child,
    pub rx: Receiver<Frame>,
    pub width: u32,
    pub height: u32,
}

impl VideoStream {
    /// Starts decoding at `source_time` (seconds into the file). Frames are emitted at
    /// `fps` on the timeline, starting at `timeline_start`, already time-stretched by `speed`.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        ctx: &egui::Context,
        path: &Path,
        source_time: f64,
        speed: f64,
        timeline_start: f64,
        width: u32,
        height: u32,
        fps: f64,
    ) -> Result<Self> {
        let vf = format!(
            "setpts=(PTS-STARTPTS)/{speed:.6},fps={fps},scale={width}:{height}:flags=bilinear"
        );
        let mut child = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{:.3}", source_time.max(0.0)), "-i"])
            .arg(path)
            .args(["-an", "-sn", "-vf", &vf, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start ffmpeg")?;
        let mut stdout = child.stdout.take().context("no stdout")?;
        // A small bound gives back-pressure: FFmpeg pauses when we are not consuming.
        let (tx, rx) = sync_channel::<Frame>(3);
        let frame_len = (width * height * 4) as usize;
        let ctx = ctx.clone();
        thread::spawn(move || {
            let mut k: u64 = 0;
            loop {
                let mut data = vec![0u8; frame_len];
                if stdout.read_exact(&mut data).is_err() {
                    break;
                }
                let frame = Frame { time: timeline_start + k as f64 / fps, data };
                if tx.send(frame).is_err() {
                    break;
                }
                ctx.request_repaint();
                k += 1;
            }
        });
        Ok(Self { child, rx, width, height })
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
