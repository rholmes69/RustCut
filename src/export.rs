//! Rendering the timeline to an MP4 with a single FFmpeg `filter_complex` graph.
//!
//! Graph shape: black canvas → for each track bottom-to-top: per-clip chains
//! (trim/speed → scale → colour → rotate → opacity → fades → time shift) overlaid,
//! or `drawtext` for text clips → H.264. Audio: per-clip chains → `amix` → AAC.

use crate::media::ffmpeg::{self, atempo_chain};
use crate::model::{interpolate, Clip, Ease, Keyframe, MediaItem, MediaKind, Project, Prop, TrackKind};
use crate::render::{filters, layer_size, text};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct ExportSettings {
    pub path: PathBuf,
    /// Short side of the output in pixels (720, 1080, …).
    pub short_side: u32,
    pub fps: u32,
    pub crf: u8,
    pub preset: String,
}

fn num(v: f64) -> String {
    format!("{v:.4}")
}

fn even(v: f64) -> u32 {
    let n = (v.round().max(2.0)) as u32;
    n + n % 2
}

fn hex(c: [u8; 4]) -> String {
    format!("0x{:02X}{:02X}{:02X}{:02X}", c[0], c[1], c[2], c[3])
}

/// FFmpeg expression for sorted keyframes as a function of the time expression `var`.
/// Mirrors [`crate::model::interpolate`] exactly, easing included.
pub fn keyframe_expr(keys: &[Keyframe], var: &str) -> String {
    let Some(first) = keys.first() else { return "0".into() };
    if keys.len() == 1 {
        return format!("{:.5}", first.value);
    }
    // Segment i runs from keys[i] to keys[i + 1]; past the end it clamps to the last value.
    let segment = |i: usize| {
        let (a, b) = (keys[i], keys[i + 1]);
        let p = format!("clip(({var}-{:.5})/{:.5},0,1)", a.time, (b.time - a.time).max(1e-6));
        let eased = match a.ease {
            Ease::Linear => p,
            Ease::Smooth => format!("({p})*({p})*(3-2*({p}))"),
            Ease::EaseIn => format!("({p})*({p})"),
            Ease::EaseOut => format!("({p})*(2-({p}))"),
            Ease::Hold => "0".into(),
        };
        format!("{:.5}+({:.5})*{eased}", a.value, b.value - a.value)
    };
    // A balanced search over segments keeps nesting at log2(n), so long beat-synced
    // animations stay within FFmpeg's expression limits.
    fn tree(lo: usize, hi: usize, keys: &[Keyframe], var: &str, segment: &dyn Fn(usize) -> String) -> String {
        if hi - lo == 1 {
            return segment(lo);
        }
        let mid = (lo + hi) / 2;
        format!("if(lt({var},{:.5}),{},{})", keys[mid].time, tree(lo, mid, keys, var, segment), tree(mid, hi, keys, var, segment))
    }
    let body = tree(0, keys.len() - 1, keys, var, &segment);
    format!("if(lt({var},{:.5}),{:.5},{body})", first.time, first.value)
}

/// `-filter_complex <graph>`, or for long graphs (Windows limits a command line to
/// ~32 000 characters) `-/filter_complex <file>` with the graph written to `work`.
pub fn filter_complex_args(filter: String, work: &Path) -> Result<Vec<String>> {
    if filter.len() < 8000 {
        return Ok(vec!["-filter_complex".into(), filter]);
    }
    std::fs::create_dir_all(work)?;
    let mut n = 0;
    let mut path = work.join("graph.txt");
    while path.exists() {
        n += 1;
        path = work.join(format!("graph{n}.txt"));
    }
    std::fs::write(&path, filter)?;
    Ok(vec!["-/filter_complex".into(), path.to_string_lossy().into_owned()])
}

/// A property as an expression of clip-local time `var` (a constant if not animated).
fn prop_expr(clip: &Clip, prop: Prop, var: &str) -> String {
    match clip.keyframes.get(&prop) {
        Some(keys) if !keys.is_empty() => keyframe_expr(keys, var),
        _ => format!("{:.5}", prop.base(clip)),
    }
}

/// `sendcmd` script that drives an animated opacity once per output frame, for a
/// stream covering clip-local time `local_start .. local_start + len`.
fn opacity_commands(clip: &Clip, target: &str, fps: u32, local_start: f64, len: f64) -> String {
    let keys = &clip.keyframes[&Prop::Opacity];
    let frames = (len * fps as f64).ceil() as u64;
    let mut out = String::new();
    let mut last = f32::NAN;
    for i in 0..=frames {
        let st = i as f64 / fps as f64;
        let v = interpolate(keys, local_start + st).clamp(0.0, 1.0);
        if (v - last).abs() > 0.001 || last.is_nan() {
            out += &format!("{st:.4} {target} aa {v:.4};\n");
            last = v;
        }
    }
    out
}

/// `sendcmd` script setting `target`'s `param` to `map(value)` once per output frame
/// for an animated property.
fn prop_commands(clip: &Clip, prop: Prop, target: &str, param: &str, fps: u32, local_start: f64, len: f64, map: &dyn Fn(f32) -> f64) -> String {
    let keys = &clip.keyframes[&prop];
    let frames = (len * fps as f64).ceil() as u64;
    let mut out = String::new();
    let mut last = f64::NAN;
    for i in 0..=frames {
        let st = i as f64 / fps as f64;
        let v = map(interpolate(keys, local_start + st));
        if (v - last).abs() > 0.002 || last.is_nan() {
            out += &format!("{st:.4} {target} {param} {v:.4};\n");
            last = v;
        }
    }
    out
}

/// Blur and glow filters for a media layer (empty when neither is used).
fn effect_filters(clip: &Clip, short: f64, fps: u32, work_dir: &Path, local_start: f64, len: f64, tag: &str) -> Result<Vec<String>> {
    let mut chain = Vec::new();
    let px = short / 1080.0;
    // Glow: lift brightness and saturation (eq), driven per frame when animated.
    let glow_eq = |g: f32| (0.22 * g.clamp(0.0, 1.0) as f64, 1.0 + 0.4 * g.clamp(0.0, 1.0) as f64);
    if clip.is_animated(Prop::Glow) {
        let name = format!("eq@g{tag}");
        let file = work_dir.join(format!("glow_{}_{tag}.cmd", clip.id));
        // One interval per frame setting both values (sendcmd wants times in order).
        let keys = &clip.keyframes[&Prop::Glow];
        let frames = (len * fps as f64).ceil() as u64;
        let mut cmds = String::new();
        let mut last = f32::NAN;
        for i in 0..=frames {
            let st = i as f64 / fps as f64;
            let g = interpolate(keys, local_start + st).clamp(0.0, 1.0);
            if (g - last).abs() > 0.004 || last.is_nan() {
                let (b, s) = glow_eq(g);
                cmds += &format!("{st:.4} {name} brightness {b:.4}, {name} saturation {s:.4};
");
                last = g;
            }
        }
        std::fs::write(&file, cmds)?;
        let (b, s) = glow_eq(interpolate(&clip.keyframes[&Prop::Glow], local_start));
        chain.push(format!("sendcmd=f='{}'", ffmpeg::filter_path(&file)));
        chain.push(format!("{name}=brightness={b:.4}:saturation={s:.4}"));
    } else if clip.glow > 0.001 {
        let (b, s) = glow_eq(clip.glow);
        chain.push(format!("eq=brightness={b:.4}:saturation={s:.4}"));
    }
    if clip.is_animated(Prop::Blur) {
        let name = format!("gblur@b{tag}");
        let file = work_dir.join(format!("blur_{}_{tag}.cmd", clip.id));
        std::fs::write(&file, prop_commands(clip, Prop::Blur, &name, "sigma", fps, local_start, len, &|v| (v.max(0.0) as f64 * px / 2.0).max(0.0)))?;
        let s0 = (interpolate(&clip.keyframes[&Prop::Blur], local_start).max(0.0) as f64 * px / 2.0).max(0.0);
        chain.push(format!("sendcmd=f='{}'", ffmpeg::filter_path(&file)));
        chain.push(format!("{name}=sigma={s0:.4}"));
    } else if clip.blur > 0.05 {
        chain.push(format!("gblur=sigma={:.4}", clip.blur as f64 * px / 2.0));
    }
    Ok(chain)
}

/// `asendcmd` script for many volume keyframes, sampled every 20 ms.
fn volume_commands(keys: &[Keyframe], target: &str, duration: f64) -> String {
    let steps = (duration * 50.0).ceil() as u64;
    let mut out = String::new();
    let mut last = f32::NAN;
    for i in 0..=steps {
        let t = i as f64 / 50.0;
        let v = interpolate(keys, t).max(0.0);
        if (v - last).abs() > 0.002 || last.is_nan() {
            out += &format!("{t:.3} {target} volume {v:.4};\n");
            last = v;
        }
    }
    out
}

/// Filters that turn a decoded clip stream into its transformed RGBA layer: size,
/// colour, rotation and opacity (keyframes included). `var` is the FFmpeg expression
/// for clip-local time; the stream covers `local_start .. local_start + len`.
#[allow(clippy::too_many_arguments)]
fn layer_filters(
    clip: &Clip,
    media: &MediaItem,
    canvas: (f64, f64),
    fps: u32,
    work_dir: &Path,
    var: &str,
    local_start: f64,
    len: f64,
    tag: &str,
) -> Result<Vec<String>> {
    let (fw, fh) = layer_size(media.width as f64, media.height as f64, canvas.0, canvas.1, 1.0);
    let mut chain = Vec::new();
    if clip.is_animated(Prop::Scale) {
        // Colour first (constant frame size), then a per-frame scale inside a fixed
        // transparent box so downstream filters see a constant size.
        chain.extend(filters::ffmpeg_filters(clip.filter, &clip.adjust));
        chain.extend(effect_filters(clip, canvas.0.min(canvas.1), fps, work_dir, local_start, len, tag)?);
        let s = prop_expr(clip, Prop::Scale, var);
        let smax = clip.keyframes[&Prop::Scale].iter().map(|k| k.value).fold(0.01f32, f32::max) as f64;
        chain.push(format!(
            "scale=w='max(2,trunc({fw:.3}*({s})/2)*2)':h='max(2,trunc({fh:.3}*({s})/2)*2)':eval=frame"
        ));
        chain.push("format=rgba".into());
        chain.push(format!(
            "pad=w={}:h={}:x='(ow-iw)/2':y='(oh-ih)/2':color=black@0:eval=frame",
            even(fw * smax) + 2,
            even(fh * smax) + 2
        ));
    } else {
        let s = clip.transform.scale as f64;
        chain.push(format!("scale={}:{}", even(fw * s), even(fh * s)));
        chain.extend(filters::ffmpeg_filters(clip.filter, &clip.adjust));
        chain.extend(effect_filters(clip, canvas.0.min(canvas.1), fps, work_dir, local_start, len, tag)?);
        chain.push("format=rgba".into());
    }
    if clip.is_animated(Prop::Rotation) {
        let r = prop_expr(clip, Prop::Rotation, var);
        chain.push(format!("rotate=a='({r})*PI/180':c=none:ow='hypot(iw,ih)':oh='hypot(iw,ih)'"));
    } else if clip.transform.rotation.abs() > 0.01 {
        let r = (clip.transform.rotation as f64).to_radians();
        chain.push(format!("rotate={r:.5}:c=none:ow=rotw({r:.5}):oh=roth({r:.5})"));
    }
    if clip.is_animated(Prop::Opacity) {
        let name = format!("colorchannelmixer@{tag}");
        let cmd_file = work_dir.join(format!("opacity_{}_{tag}.cmd", clip.id));
        std::fs::write(&cmd_file, opacity_commands(clip, &name, fps, local_start, len))?;
        chain.push(format!("sendcmd=f='{}'", ffmpeg::filter_path(&cmd_file)));
        chain.push(format!("{name}=aa={:.4}", interpolate(&clip.keyframes[&Prop::Opacity], local_start).clamp(0.0, 1.0)));
    } else if clip.opacity < 0.999 {
        chain.push(format!("colorchannelmixer=aa={:.3}", clip.opacity));
    }
    Ok(chain)
}

/// FFmpeg inputs plus the filter graph that composites the timeline.
pub struct Graph {
    pub inputs: Vec<String>,
    pub filter: String,
    pub video_label: String,
    pub audio_label: Option<String>,
    pub duration: f64,
}

/// Builds the compositing graph. Text clips are written to `work_dir`.
/// `duration` overrides the project length; `with_video` / `with_audio` choose
/// which outputs the graph produces.
pub fn build_graph(
    project: &Project,
    short_side: u32,
    fps: u32,
    work_dir: &Path,
    duration: Option<f64>,
    with_video: bool,
    with_audio: bool,
) -> Result<Graph> {
    let total = duration.unwrap_or_else(|| project.duration());
    if total < 0.05 {
        bail!("The timeline is empty — add some clips first.");
    }
    let (w, h) = project.aspect.size_for(short_side);
    let (wf, hf) = (w as f64, h as f64);
    let short = wf.min(hf);
    let fps = fps.max(1);

    let mut args: Vec<String> = Vec::new();
    let mut graph = Vec::new();
    if with_video {
        graph.push(format!("color=c=black:s={w}x{h}:r={fps}:d={},format=yuv420p[base0]", num(total)));
    }
    let mut base = "base0".to_string();
    let mut layer = 0usize;
    let mut inputs = 0usize;
    let mut audio_labels: Vec<String> = Vec::new();

    for (ti, track) in project.tracks.iter().enumerate().rev() {
        match track.kind {
            TrackKind::Text => {
                if track.hidden || !with_video {
                    continue;
                }
                for clip in &track.clips {
                    let Some(style) = clip.text() else { continue };
                    if style.content.trim().is_empty() {
                        continue;
                    }
                    // drawtext runs on the composite, so `t` is timeline time here.
                    let local = format!("(t-{})", num(clip.start));
                    let base_px = text::font_px(style.font_size, 1.0, short);
                    let fs = base_px * clip.transform.scale as f64;
                    let text_file = work_dir.join(format!("text_{}.txt", clip.id));
                    std::fs::write(&text_file, &style.content)?;
                    let (fi, fo) = clip.fades();
                    let mut alpha = format!("clip({},0,1)", prop_expr(clip, Prop::Opacity, &local));
                    if fi > 0.0 {
                        alpha += &format!("*clip((t-{})/{},0,1)", num(clip.start), num(fi));
                    }
                    if fo > 0.0 {
                        alpha += &format!("*clip(({}-t)/{},0,1)", num(clip.end()), num(fo));
                    }
                    let mut o = Vec::new();
                    if let Some(font) = text::font_path(style.bold) {
                        o.push(format!("fontfile='{}'", ffmpeg::filter_path(&font)));
                    }
                    o.push(format!("textfile='{}'", ffmpeg::filter_path(&text_file)));
                    // Render the text literally ("100%" must not be parsed as an expansion).
                    o.push("expansion=none".into());
                    if clip.is_animated(Prop::Scale) {
                        o.push(format!("fontsize='max(1,{base_px:.3}*({}))'", prop_expr(clip, Prop::Scale, &local)));
                    } else {
                        o.push(format!("fontsize={fs:.1}"));
                    }
                    o.push(format!("fontcolor={}", hex(style.color)));
                    // Karaoke parts start where their full line would start if centred.
                    let anchor = style.align_to.as_ref().filter(|f| !f.is_empty()).and_then(|f| text::line_width_px(f, style.bold, base_px));
                    match anchor {
                        Some(full_w) => o.push(format!("x='(w-{full_w:.2}*({}))/2+({})*{w}'", prop_expr(clip, Prop::Scale, &local), prop_expr(clip, Prop::X, &local))),
                        None => o.push(format!("x='(w-text_w)/2+({})*{w}'", prop_expr(clip, Prop::X, &local))),
                    }
                    o.push(format!("y='(h-text_h)/2+({})*{h}'", prop_expr(clip, Prop::Y, &local)));
                    o.push(format!("alpha='{alpha}'"));
                    if let Some(bg) = style.background {
                        o.push("box=1".into());
                        o.push(format!("boxcolor={}", hex(bg)));
                        o.push(format!("boxborderw={:.0}", text::box_padding(fs)));
                    }
                    if let Some(ol) = style.outline {
                        o.push(format!("borderw={:.1}", text::outline_width(fs)));
                        o.push(format!("bordercolor={}", hex(ol)));
                    }
                    o.push(format!("enable='between(t,{},{})'", num(clip.start), num(clip.end())));
                    let next = format!("base{}", layer + 1);
                    graph.push(format!("[{base}]drawtext={}[{next}]", o.join(":")));
                    base = next;
                    layer += 1;
                }
            }
            TrackKind::Video | TrackKind::Audio => {
                let video_track = with_video && track.kind == TrackKind::Video && !track.hidden;
                let transitions = if track.kind == TrackKind::Video { project.track_transitions(ti) } else { Vec::new() };
                let half_in = |id| transitions.iter().find(|t| t.to == id).map_or(0.0, |t| t.duration / 2.0);
                let half_out = |id| transitions.iter().find(|t| t.from == id).map_or(0.0, |t| t.duration / 2.0);

                for clip in &track.clips {
                    let Some(media) = clip.media_id().and_then(|m| project.media(m)) else { continue };
                    let want_video = video_track && media.has_video();
                    let audible = clip.volume > 0.001 || clip.is_animated(Prop::Volume);
                    let want_audio = with_audio && !track.muted && media.has_sound() && audible;
                    if !want_video && !want_audio {
                        continue;
                    }
                    let is_image = media.kind == MediaKind::Image;
                    if is_image {
                        args.extend(["-loop".into(), "1".into(), "-framerate".into(), fps.to_string()]);
                        args.extend(["-t".into(), num(clip.duration)]);
                    } else {
                        args.extend(["-ss".into(), num(clip.source_in), "-t".into(), num(clip.duration * clip.speed)]);
                    }
                    args.push("-i".into());
                    args.push(media.path.to_string_lossy().into_owned());
                    let idx = inputs;
                    inputs += 1;
                    let (fi, fo) = clip.fades();
                    let (hin, hout) = (half_in(clip.id), half_out(clip.id));

                    if let Some(plate) = clip.background_plate.and_then(|m| project.media(m)).filter(|_| want_video) {
                        args.extend(["-loop".into(), "1".into(), "-framerate".into(), fps.to_string()]);
                        args.extend(["-t".into(), num(clip.duration), "-i".into(), plate.path.to_string_lossy().into_owned()]);
                        let pidx = inputs;
                        inputs += 1;
                        let p = format!("plate{layer}");
                        graph.push(format!(
                            "[{pidx}:v]setpts=PTS-STARTPTS,scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},format=rgba,setpts=PTS+{}/TB[{p}]",
                            num(clip.start)
                        ));
                        let next = format!("base{}", layer + 1);
                        graph.push(format!(
                            "[{base}][{p}]overlay=x=0:y=0:eof_action=pass:enable='between(t,{},{})'[{next}]",
                            num(clip.start + hin),
                            num(clip.end() - hout)
                        ));
                        base = next;
                        layer += 1;
                    }

                    if want_video {
                        // Inside the per-clip chain `t` is clip-local time.
                        let mut chain = vec![if is_image {
                            "setpts=PTS-STARTPTS".to_string()
                        } else {
                            format!("setpts=(PTS-STARTPTS)/{}", num(clip.speed))
                        }];
                        chain.extend(layer_filters(
                            clip,
                            media,
                            (wf, hf),
                            fps,
                            work_dir,
                            "t",
                            0.0,
                            clip.duration,
                            &format!("op{layer}"),
                        )?);
                        if fi > 0.0 {
                            chain.push(format!("fade=t=in:st=0:d={}:alpha=1", num(fi)));
                        }
                        if fo > 0.0 {
                            chain.push(format!("fade=t=out:st={}:d={}:alpha=1", num(clip.duration - fo), num(fo)));
                        }
                        chain.push(format!("setpts=PTS+{}/TB", num(clip.start)));
                        let v = format!("v{layer}");
                        graph.push(format!("[{idx}:v]{}[{v}]", chain.join(",")));
                        // overlay runs on the composite: timeline time. Transition windows are
                        // drawn by the transition layer instead.
                        let local = format!("(t-{})", num(clip.start));
                        let next = format!("base{}", layer + 1);
                        graph.push(format!(
                            "[{base}][{v}]overlay=x='(W-w)/2+({})*{w}':y='(H-h)/2+({})*{h}':eval=frame:eof_action=pass:enable='between(t,{},{})'[{next}]",
                            prop_expr(clip, Prop::X, &local),
                            prop_expr(clip, Prop::Y, &local),
                            num(clip.start + hin),
                            num(clip.end() - hout),
                        ));
                        base = next;
                        layer += 1;
                    }

                    if want_audio {
                        let mut chain = vec!["asetpts=PTS-STARTPTS".to_string()];
                        if let Some(a) = atempo_chain(clip.speed) {
                            chain.push(a);
                        }
                        if let Some(d) = &clip.denoise {
                            chain.push(crate::smart_audio::denoise_filter(d));
                        }
                        let volume_keys = clip.keyframes.get(&Prop::Volume).filter(|k| !k.is_empty());
                        if let Some(keys) = volume_keys.filter(|k| k.len() > 12) {
                            // Many keyframes (e.g. auto-ducking): drive the volume with a command
                            // script instead of one huge expression.
                            let name = format!("volume@vk{}", audio_labels.len());
                            let file = work_dir.join(format!("volume_{}.cmd", clip.id));
                            std::fs::write(&file, volume_commands(keys, &name, clip.duration))?;
                            chain.push(format!("asendcmd=f='{}'", ffmpeg::filter_path(&file)));
                            chain.push(format!("{name}=volume={:.4}:eval=frame", interpolate(keys, 0.0).max(0.0)));
                        } else if volume_keys.is_some() {
                            chain.push(format!("volume='max(0,{})':eval=frame", prop_expr(clip, Prop::Volume, "t")));
                        } else {
                            chain.push(format!("volume={:.3}", clip.volume));
                        }
                        // Soften the sound across video transitions.
                        let (afi, afo) = (fi.max(hin), fo.max(hout));
                        if afi > 0.0 {
                            chain.push(format!("afade=t=in:st=0:d={}", num(afi)));
                        }
                        if afo > 0.0 {
                            chain.push(format!("afade=t=out:st={}:d={}", num(clip.duration - afo), num(afo)));
                        }
                        chain.push("aformat=sample_rates=48000:channel_layouts=stereo".into());
                        chain.push(format!("adelay={}:all=1", (clip.start * 1000.0).round() as u64));
                        let a = format!("a{}", audio_labels.len());
                        graph.push(format!("[{idx}:a]{}[{a}]", chain.join(",")));
                        audio_labels.push(a);
                    }
                }

                if !video_track {
                    continue;
                }
                // Transitions: render both sides across the window onto transparent canvases
                // and blend them with xfade.
                for tr in &transitions {
                    let (Some(a), Some(b)) = (project.clip(tr.from), project.clip(tr.to)) else { continue };
                    let (Some(ma), Some(mb)) = (
                        a.media_id().and_then(|m| project.media(m)).filter(|m| m.has_video()),
                        b.media_id().and_then(|m| project.media(m)).filter(|m| m.has_video()),
                    ) else {
                        continue;
                    };
                    let d = tr.duration;
                    let n = layer;
                    let mut sides = Vec::new();
                    for (side, clip, media, local_start) in
                        [("a", a, ma, a.duration - d / 2.0), ("b", b, mb, -d / 2.0)]
                    {
                        let is_image = media.kind == MediaKind::Image;
                        let mut pre = 0.0;
                        if is_image {
                            args.extend(["-loop".into(), "1".into(), "-framerate".into(), fps.to_string()]);
                            args.extend(["-t".into(), num(d)]);
                        } else {
                            let mut src = clip.source_in + local_start * clip.speed;
                            if src < 0.0 {
                                pre = -src / clip.speed;
                                src = 0.0;
                            }
                            args.extend(["-ss".into(), num(src), "-t".into(), num(d * clip.speed + 0.2)]);
                        }
                        args.push("-i".into());
                        args.push(media.path.to_string_lossy().into_owned());
                        let idx = inputs;
                        inputs += 1;

                        let mut chain = vec![
                            if is_image {
                                "setpts=PTS-STARTPTS".to_string()
                            } else {
                                format!("setpts=(PTS-STARTPTS)/{}", num(clip.speed))
                            },
                            format!("fps={fps}"),
                        ];
                        // Hold the first/last frame when the source has no spare footage.
                        if pre > 0.0 {
                            chain.push(format!("tpad=start_mode=clone:start_duration={}", num(pre)));
                        }
                        chain.push(format!("tpad=stop_mode=clone:stop_duration={}", num(d)));
                        chain.push(format!("trim=duration={}", num(d)));
                        chain.push("setpts=PTS-STARTPTS".into());
                        let var = format!("(t+{})", num(local_start));
                        chain.extend(layer_filters(
                            clip,
                            media,
                            (wf, hf),
                            fps,
                            work_dir,
                            &var,
                            local_start,
                            d,
                            &format!("opt{n}{side}"),
                        )?);
                        let seg = format!("ts{n}{side}");
                        graph.push(format!("[{idx}:v]{}[{seg}]", chain.join(",")));
                        graph.push(format!("color=c=black@0:s={w}x{h}:r={fps}:d={},format=rgba[tc{n}{side}]", num(d)));
                        graph.push(format!(
                            "[tc{n}{side}][{seg}]overlay=x='(W-w)/2+({})*{w}':y='(H-h)/2+({})*{h}':eval=frame:format=auto,format=rgba[tl{n}{side}]",
                            prop_expr(clip, Prop::X, &var),
                            prop_expr(clip, Prop::Y, &var),
                        ));
                        sides.push(format!("tl{n}{side}"));
                    }
                    graph.push(format!(
                        "[{}][{}]xfade=transition={}:duration={}:offset=0,setpts=PTS+{}/TB[tx{n}]",
                        sides[0],
                        sides[1],
                        tr.kind.xfade(),
                        num(d),
                        num(tr.start())
                    ));
                    let next = format!("base{}", layer + 1);
                    graph.push(format!(
                        "[{base}][tx{n}]overlay=x=0:y=0:eof_action=pass:enable='between(t,{},{})'[{next}]",
                        num(tr.start()),
                        num(tr.end())
                    ));
                    base = next;
                    layer += 1;
                }
            }
        }
    }

    let audio_label = with_audio.then(|| {
        graph.push(format!("anullsrc=r=48000:cl=stereo,atrim=0:{}[asil]", num(total)));
        if audio_labels.is_empty() {
            graph.push("[asil]anull[amix]".into());
        } else {
            let ins: String = audio_labels.iter().map(|a| format!("[{a}]")).collect();
            graph.push(format!(
                "[asil]{ins}amix=inputs={}:duration=first:normalize=0[amix]",
                audio_labels.len() + 1
            ));
        }
        match (project.audio.master_gain_db, project.audio.loudness) {
            // Measured gain + true-peak limiter (exports): accurate on short projects too.
            (Some(gain), _) => graph.push(format!("[amix]{}[aout]", crate::smart_audio::master_filter(gain))),
            // Single-pass fallback when no measurement was made.
            (None, Some(target)) => graph.push(format!("[amix]{}[aout]", crate::smart_audio::loudnorm_filter(target))),
            (None, None) => graph.push("[amix]anull[aout]".into()),
        }
        "aout".to_string()
    });
    Ok(Graph { inputs: args, filter: graph.join(";"), video_label: base, audio_label, duration: total })
}

/// Builds the full FFmpeg argument list for an MP4 export.
pub fn build_args(project: &Project, s: &ExportSettings, work_dir: &Path) -> Result<Vec<String>> {
    let fps = s.fps.max(1);
    let g = build_graph(project, s.short_side, fps, work_dir, None, true, true)?;
    let mut args: Vec<String> = ["-hide_banner", "-y", "-loglevel", "error", "-nostats", "-progress", "pipe:1"]
        .map(String::from)
        .to_vec();
    args.extend(g.inputs);
    args.extend(filter_complex_args(g.filter, work_dir)?);
    args.extend([
        "-map".into(),
        format!("[{}]", g.video_label),
        "-map".into(),
        format!("[{}]", g.audio_label.unwrap_or_default()),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        s.preset.clone(),
        "-crf".into(),
        s.crf.to_string(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-r".into(),
        fps.to_string(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-t".into(),
        num(g.duration),
        "-movflags".into(),
        "+faststart".into(),
        s.path.to_string_lossy().into_owned(),
    ]);
    Ok(args)
}

/// Renders the composited frame at timeline time `t` as a PNG.
///
/// The timeline is cut down to a few seconds around `t` (clips are split, not
/// re-timed, so keyframes, fades and transitions stay exact) and re-based to start
/// near zero, which keeps this fast no matter how far into the timeline `t` is.
pub fn render_frame_png(project: &Project, t: f64, short_side: u32) -> Result<Vec<u8>> {
    const MARGIN: f64 = 3.0;
    let mut snap = project.clone();
    let (lo, hi) = (t - MARGIN, t + MARGIN);
    let ids: Vec<_> = snap.tracks.iter().flat_map(|tr| tr.clips.iter().map(|c| c.id)).collect();
    for id in ids {
        let Some(c) = snap.clip(id).cloned() else { continue };
        if c.end() <= lo || c.start >= hi {
            snap.remove_clip(id);
        } else if c.start < lo && lo > c.start + c.fade_in + 0.01 {
            // Drop the part before the window (never inside a fade-in, which a split resets).
            if snap.split_clip(id, lo).is_some() {
                snap.remove_clip(id);
            }
        }
    }
    let origin = snap.tracks.iter().flat_map(|tr| &tr.clips).map(|c| c.start).fold(lo, f64::min).max(0.0);
    for c in snap.tracks.iter_mut().flat_map(|tr| tr.clips.iter_mut()) {
        c.start -= origin;
    }
    let at = (t - origin).max(0.0);
    let work = ffmpeg::work_dir("frame")?;
    let result = (|| -> Result<Vec<u8>> {
        let g = build_graph(&snap, short_side, snap.fps, &work, Some(at + 0.5), true, false)?;
        let out = ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error"])
            .args(&g.inputs)
            .args(filter_complex_args(g.filter.clone(), &work)?)
            .args(["-map", &format!("[{}]", g.video_label)])
            .args(["-ss", &format!("{at:.4}"), "-frames:v", "1", "-c:v", "png", "-f", "image2pipe", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("could not start ffmpeg")?;
        if !out.status.success() || out.stdout.is_empty() {
            bail!("FFmpeg failed to render the frame: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(out.stdout)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Runs an export to completion. Returns `Ok(false)` when cancelled.
pub fn run(
    project: &Project,
    settings: &ExportSettings,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f32),
) -> Result<bool> {
    let work = ffmpeg::work_dir("export")?;
    let result = (|| -> Result<bool> {
        // Loudness normalisation: measure the mix first and bake the exact gain in.
        let mut normalised;
        let project = match project.audio.loudness {
            Some(target) => {
                on_progress(0.0);
                normalised = project.clone();
                normalised.audio.master_gain_db = Some(crate::smart_audio::normalization_gain(project, target)?);
                &normalised
            }
            None => project,
        };
        let args = build_args(project, settings, &work)?;
        let finished = ffmpeg::run_with_progress(&args, project.duration(), cancel, &mut on_progress)?;
        if !finished {
            let _ = std::fs::remove_file(&settings.path);
            return Ok(false);
        }
        on_progress(1.0);
        Ok(true)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

#[derive(Clone, Debug)]
pub enum ExportState {
    Running(f32),
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

/// An export running on a background thread.
pub struct ExportJob {
    state: Arc<Mutex<ExportState>>,
    cancel: Arc<AtomicBool>,
    pub started: Instant,
    pub finished: Arc<Mutex<Option<Instant>>>,
}

impl ExportJob {
    pub fn start(project: Project, settings: ExportSettings, ctx: egui::Context) -> Self {
        let state = Arc::new(Mutex::new(ExportState::Running(0.0)));
        let cancel = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(Mutex::new(None));
        let (st, cn, fin) = (state.clone(), cancel.clone(), finished.clone());
        std::thread::spawn(move || {
            let progress_state = st.clone();
            let progress_ctx = ctx.clone();
            let result = run(&project, &settings, &cn, move |p| {
                *progress_state.lock().unwrap() = ExportState::Running(p);
                progress_ctx.request_repaint();
            });
            *st.lock().unwrap() = match result {
                Ok(true) => ExportState::Done(settings.path.clone()),
                Ok(false) => ExportState::Cancelled,
                Err(e) => ExportState::Failed(format!("{e:#}")),
            };
            *fin.lock().unwrap() = Some(Instant::now());
            ctx.request_repaint();
        });
        Self { state, cancel, started: Instant::now(), finished }
    }

    pub fn state(&self) -> ExportState {
        self.state.lock().unwrap().clone()
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state(), ExportState::Running(_))
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn elapsed(&self) -> std::time::Duration {
        match *self.finished.lock().unwrap() {
            Some(end) => end - self.started,
            None => self.started.elapsed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn project() -> Project {
        let mut p = Project::default();
        let v = p.add_media(MediaItem {
            id: 0,
            path: "C:/media/a.mp4".into(),
            name: "a.mp4".into(),
            kind: MediaKind::Video,
            duration: 10.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: true,
        });
        let mut c = p.new_media_clip(v, 0.0).unwrap();
        c.fade_in = 1.0;
        c.speed = 2.0;
        c.duration = 5.0;
        c.filter = FilterPreset::Vivid;
        p.append_to_main(c);
        let mut t = p.new_text_clip(TextStyle::default(), 1.0);
        t.transform.y = 0.3;
        p.place_clip(t, None);
        p
    }

    fn settings() -> ExportSettings {
        ExportSettings { path: "out.mp4".into(), short_side: 720, fps: 30, crf: 23, preset: "veryfast".into() }
    }

    #[test]
    fn graph_contains_every_layer() {
        let dir = std::env::temp_dir().join("rustcut-test-graph");
        std::fs::create_dir_all(&dir).unwrap();
        let args = build_args(&project(), &settings(), &dir).unwrap();
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.starts_with("color=c=black:s=1280x720"));
        assert!(graph.contains("setpts=(PTS-STARTPTS)/2.0000"));
        assert!(graph.contains("scale=1280:720"));
        assert!(graph.contains("eq=brightness=0.000:contrast=1.080:saturation=1.450"));
        assert!(graph.contains("fade=t=in:st=0:d=1.0000:alpha=1"));
        assert!(graph.contains("drawtext="));
        assert!(graph.contains("y='(h-text_h)/2+(0.30000)*720'"));
        assert!(graph.contains("atempo=2.00000"));
        assert!(graph.contains("amix=inputs=2"));
        // Text (top track) is drawn after video (main track).
        assert!(graph.find("overlay").unwrap() < graph.find("drawtext").unwrap());
        assert_eq!(args.iter().filter(|a| *a == "-i").count(), 1);
        let ss = args.iter().position(|a| a == "-ss").unwrap();
        assert_eq!(args[ss + 3], "10.0000"); // -t is the source length (5 s × 2)
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Evaluates the subset of FFmpeg's expression language that `keyframe_expr` emits.
    fn eval(e: &str, t: f64) -> f64 {
        fn parse(s: &[u8], i: &mut usize, t: f64) -> f64 {
            let mut v = term(s, i, t);
            while *i < s.len() && (s[*i] == b'+' || s[*i] == b'-') {
                let op = s[*i];
                *i += 1;
                let r = term(s, i, t);
                v = if op == b'+' { v + r } else { v - r };
            }
            v
        }
        fn term(s: &[u8], i: &mut usize, t: f64) -> f64 {
            let mut v = atom(s, i, t);
            while *i < s.len() && (s[*i] == b'*' || s[*i] == b'/') {
                let op = s[*i];
                *i += 1;
                let r = atom(s, i, t);
                v = if op == b'*' { v * r } else { v / r };
            }
            v
        }
        fn args(s: &[u8], i: &mut usize, t: f64) -> Vec<f64> {
            let mut out = vec![parse(s, i, t)];
            while s[*i] == b',' {
                *i += 1;
                out.push(parse(s, i, t));
            }
            *i += 1; // ')'
            out
        }
        fn atom(s: &[u8], i: &mut usize, t: f64) -> f64 {
            let rest = std::str::from_utf8(&s[*i..]).unwrap();
            if rest.starts_with('(') {
                *i += 1;
                let v = parse(s, i, t);
                *i += 1;
                v
            } else if rest.starts_with('-') {
                *i += 1;
                -atom(s, i, t)
            } else if rest.starts_with("if(") {
                *i += 3;
                let a = args(s, i, t);
                if a[0] != 0.0 { a[1] } else { a[2] }
            } else if rest.starts_with("lt(") {
                *i += 3;
                let a = args(s, i, t);
                (a[0] < a[1]) as i32 as f64
            } else if rest.starts_with("clip(") {
                *i += 5;
                let a = args(s, i, t);
                a[0].clamp(a[1], a[2])
            } else if rest.starts_with('t') {
                *i += 1;
                t
            } else {
                let n = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
                *i += n;
                rest[..n].parse().unwrap()
            }
        }
        let mut i = 0;
        parse(e.as_bytes(), &mut i, t)
    }

    #[test]
    fn keyframe_expressions_match_preview() {
        let keys = vec![
            Keyframe { time: 0.5, value: 1.0, ease: Ease::Smooth },
            Keyframe { time: 2.0, value: 2.5, ease: Ease::EaseOut },
            Keyframe { time: 3.0, value: -1.0, ease: Ease::Hold },
            Keyframe { time: 3.5, value: 0.0, ease: Ease::EaseIn },
            Keyframe { time: 4.0, value: 1.0, ease: Ease::Linear },
        ];
        let e = keyframe_expr(&keys, "t");
        for i in 0..=50 {
            let t = i as f64 * 0.1;
            let want = interpolate(&keys, t) as f64;
            assert!((eval(&e, t) - want).abs() < 1e-3, "t={t}: expr {} vs preview {want}", eval(&e, t));
        }
    }

    #[test]
    fn animated_clip_uses_per_frame_filters() {
        let mut p = project();
        let main = p.main_track();
        let id = p.tracks[main].clips[0].id;
        let c = p.clip_mut(id).unwrap();
        c.set_key(Prop::Scale, 0.0, 1.0);
        c.set_key(Prop::Scale, 2.0, 1.5);
        c.set_key(Prop::Rotation, 1.0, 45.0);
        c.set_key(Prop::Opacity, 0.0, 0.0);
        c.set_key(Prop::Opacity, 1.0, 1.0);
        c.set_key(Prop::X, 0.0, -0.5);
        c.set_key(Prop::X, 2.0, 0.0);
        c.set_key(Prop::Volume, 0.0, 0.0);
        c.set_key(Prop::Volume, 1.0, 1.0);
        let dir = std::env::temp_dir().join("rustcut-test-kf");
        std::fs::create_dir_all(&dir).unwrap();
        let args = build_args(&p, &settings(), &dir).unwrap();
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.contains(":eval=frame,format=rgba,pad=w=1922:h=1082:"), "{graph}");
        assert!(graph.contains("rotate=a='(45.00000)*PI/180'"));
        assert!(graph.contains("sendcmd=f='"));
        assert!(graph.contains("colorchannelmixer@op0=aa=0.0000"));
        assert!(graph.contains("overlay=x='(W-w)/2+(if(lt((t-0.0000),0.00000)"));
        assert!(graph.contains("volume='max(0,if(lt(t,0.00000)"));
        let cmd = std::fs::read_to_string(dir.join(format!("opacity_{id}_op0.cmd"))).unwrap();
        assert!(cmd.starts_with("0.0000 colorchannelmixer@op0 aa 0.0000;"));
        assert!(cmd.contains("1.0000 colorchannelmixer@op0 aa 1.0000;"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transition_graph() {
        let mut p = project();
        let main = p.main_track();
        let first = p.tracks[main].clips[0].id;
        let v = p.tracks[main].clips[0].media_id().unwrap();
        let mut second = p.new_media_clip(v, 0.0).unwrap();
        second.duration = 4.0;
        let second_id = second.id;
        p.append_to_main(second);
        p.set_transition(second_id, Some(Transition { kind: TransitionKind::WipeLeft, duration: 1.0 })).unwrap();
        let dir = std::env::temp_dir().join("rustcut-test-tr");
        std::fs::create_dir_all(&dir).unwrap();
        let args = build_args(&p, &settings(), &dir).unwrap();
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        // Normal layers stop/start at the window edges (cut at 5 s, window 4.5–5.5 s).
        assert!(graph.contains("enable='between(t,0.0000,4.5000)'"), "{graph}");
        assert!(graph.contains("enable='between(t,5.5000,9.0000)'"));
        // The outgoing side reads past its out point; the incoming side reads before its in
        // point, which does not exist here, so its first frame is held.
        assert!(graph.contains("tpad=start_mode=clone:start_duration=0.5000"));
        assert!(graph.contains("xfade=transition=wipeleft:duration=1.0000:offset=0,setpts=PTS+4.5000/TB"));
        assert!(graph.contains("enable='between(t,4.5000,5.5000)'"));
        // 2 normal inputs + 2 transition inputs.
        assert_eq!(args.iter().filter(|a| *a == "-i").count(), 4);
        let _ = first;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn smart_audio_in_graph() {
        let mut p = project();
        p.audio.loudness = Some(-14.0);
        let main = p.main_track();
        let id = p.tracks[main].clips[0].id;
        let c = p.clip_mut(id).unwrap();
        c.denoise = Some(Denoise { level: NoiseLevel::Strong, floor_db: -40.0 });
        for i in 0..20 {
            c.set_key(Prop::Volume, i as f64 * 0.25, if i % 2 == 0 { 1.0 } else { 0.25 });
        }
        let dir = std::env::temp_dir().join("rustcut-test-sa");
        std::fs::create_dir_all(&dir).unwrap();
        let args = build_args(&p, &settings(), &dir).unwrap();
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("afftdn=nr=36:nf=-37.0,asendcmd=f='"), "{graph}");
        assert!(graph.contains("volume@vk0=volume=1.0000:eval=frame"));
        assert!(graph.contains("[amix]loudnorm=I=-14.0:TP=-1.5:LRA=11,aresample=48000[aout]"));
        let cmd = std::fs::read_to_string(dir.join(format!("volume_{id}.cmd"))).unwrap();
        assert!(cmd.starts_with("0.000 volume@vk0 volume 1.0000;"));
        assert!(cmd.lines().count() > 20);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn background_plate_is_drawn_behind_the_clip() {
        let mut p = project();
        let plate = p.add_media(MediaItem {
            id: 0, path: "C:/media/plate.png".into(), name: "plate.png".into(), kind: MediaKind::Image,
            duration: 0.0, width: 1080, height: 1920, fps: 0.0, has_audio: false,
        });
        p.aspect = Aspect::Portrait;
        let main = p.main_track();
        let id = p.tracks[main].clips[0].id;
        p.clip_mut(id).unwrap().background_plate = Some(plate);
        let args = build_args(&p, &settings(), &std::env::temp_dir()).unwrap();
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("scale=720:1280:force_original_aspect_ratio=increase,crop=720:1280"), "{graph}");
        let plate_at = graph.find("[plate0]overlay").unwrap();
        let clip_at = graph.find("overlay=x='(W-w)/2").unwrap();
        assert!(plate_at < clip_at, "plate must be composited first");
        assert!(args.iter().any(|a| a == "C:/media/plate.png"));
    }

    #[test]
    fn empty_timeline_is_an_error() {
        let p = Project::default();
        assert!(build_args(&p, &settings(), &std::env::temp_dir()).is_err());
    }

    #[test]
    fn muted_track_has_no_audio() {
        let mut p = project();
        let main = p.main_track();
        p.tracks[main].muted = true;
        let args = build_args(&p, &settings(), &std::env::temp_dir()).unwrap();
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("[asil]anull[amix]"));
    }
}
