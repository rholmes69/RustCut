// Builds "<Title> - lyric video.rcproj" from the music-video project:
// background (Main) + dimmed Ken Burns stills with dissolves (Scenes) + lyrics (Lyrics),
// intro card over the start, outro card after the song. Usage: node build-lyric-video.js "<project folder>/lyric-scenes.json"
// Optional cfg keys: font, fontSize, textColor [r,g,b,a].
// The visualizer is added afterwards with the MCP add_visualizer tool.
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

// Paths in the scenes file are relative to the folder that holds it (the project folder).
const cfgPath = path.resolve(process.argv[2]);
const dir = path.dirname(cfgPath);
const cfg = JSON.parse(fs.readFileSync(cfgPath, "utf8"));
const mv = JSON.parse(fs.readFileSync(path.join(dir, cfg.musicVideo), "utf8"));
const out = path.join(dir, cfg.output);

const probe = (file) => {
  const j = JSON.parse(execFileSync("ffprobe", ["-v", "error", "-print_format", "json", "-show_streams", "-show_format", file]).toString());
  const v = j.streams.find((s) => s.codec_type === "video");
  const a = j.streams.some((s) => s.codec_type === "audio");
  const [n, d] = (v && v.avg_frame_rate ? v.avg_frame_rate : "0/1").split("/").map(Number);
  return { width: v ? v.width : 0, height: v ? v.height : 0, fps: d ? n / d : 0, duration: Number(j.format.duration || 0), has_audio: a };
};

let nextId = 1;
const id = () => nextId++;
const media = [];
const addMedia = (file, kind) => {
  const abs = path.isAbsolute(file) ? file : path.join(dir, file);
  const p = probe(abs);
  const m = { id: id(), path: abs, name: path.basename(abs), kind, duration: kind === "Image" ? 0 : p.duration, width: p.width, height: p.height, fps: kind === "Image" ? 0 : p.fps, has_audio: p.has_audio };
  media.push(m);
  return m;
};
const baseClip = (start, duration, source) => ({
  id: id(), start, duration, source_in: 0, speed: 1, source,
  transform: { x: 0, y: 0, scale: 1, rotation: 0 }, opacity: 1, volume: 1, fade_in: 0, fade_out: 0,
  filter: "None", adjust: { brightness: 0, contrast: 1, saturation: 1 }, audio_only: false, audio_role: "auto",
});

// Song (keep the music-video's beat analysis, re-keyed to the new media id).
const mvSongClip = mv.tracks.find((t) => t.kind === "Audio").clips[0];
const mvSong = mv.media.find((m) => m.id === mvSongClip.source.Media);
const song = addMedia(mvSong.path, "Audio");
const songLen = mvSongClip.duration;

const bg = addMedia(cfg.background, "Video");
const intro = addMedia(cfg.intro, "Video");
const outro = addMedia(cfg.outro, "Video");

// Lyrics: same timing as the music video, restyled.
const lyricTrack = { id: id(), kind: "Text", name: "Lyrics", muted: false, hidden: false, locked: false, captions: true, clips: [] };
const mvLyrics = mv.tracks.find((t) => t.kind === "Text").clips;
for (const c of mvLyrics) {
  const t = c.source.Text;
  const cl = baseClip(c.start, c.duration, { Text: { ...t, font: cfg.font || "playfair-display", font_size: cfg.fontSize || 70, bold: false, color: cfg.textColor || [245, 236, 222, 255], background: null, outline: [0, 0, 0, 170] } });
  cl.fade_in = 0.35;
  cl.fade_out = 0.35;
  if (c.keyframes) cl.keyframes = c.keyframes;
  if (c.transform) cl.transform = { ...c.transform, x: 0, y: 0 };
  lyricTrack.clips.push(cl);
}

// Scenes: dimmed, softly blurred stills with a slow Ken Burns drift and dissolves.
const scenes = { id: id(), kind: "Video", name: "Scenes", muted: false, hidden: false, locked: false, captions: false, clips: [] };
scenes.clips.push(baseClip(0, intro.duration, { Media: intro.id }));
cfg.scenes.forEach((s, i) => {
  const m = addMedia(s.still, "Image");
  const dur = s.end - s.start;
  const c = baseClip(s.start, dur, { Media: m.id });
  c.opacity = 0.62;
  c.volume = 0;
  c.blur = 3;
  c.adjust = { brightness: -0.12, contrast: 1.05, saturation: 0.9 };
  const dirX = i % 2 === 0 ? 1 : -1;
  c.keyframes = {
    x: [{ time: 0, value: -0.02 * dirX, ease: "smooth" }, { time: dur, value: 0.02 * dirX, ease: "linear" }],
    scale: [{ time: 0, value: 1, ease: "smooth" }, { time: dur, value: 1.15, ease: "linear" }],
  };
  if (i === 0) c.fade_in = 1.5;
  else c.transition_in = { kind: "dissolve", duration: cfg.dissolve };
  scenes.clips.push(c);
});

const main = { id: id(), kind: "Video", name: "Main", muted: false, hidden: false, locked: false, captions: false, clips: [] };
main.clips.push(baseClip(0, songLen, { Media: bg.id }));
main.clips.push(baseClip(songLen, outro.duration, { Media: outro.id }));
main.clips[1].transition_in = { kind: "dip_black", duration: cfg.endDip };

const audio = { id: id(), kind: "Audio", name: "Audio 1", muted: false, hidden: false, locked: false, captions: false, clips: [] };
audio.clips.push(baseClip(mvSongClip.start, songLen, { Media: song.id }));

const music = {};
const analysis = mv.music[String(mvSong.id)] || Object.values(mv.music)[0];
if (analysis) music[String(song.id)] = analysis;

const project = { name: cfg.name, aspect: mv.aspect, fps: mv.fps, media, tracks: [lyricTrack, scenes, main, audio], next_id: nextId, audio: mv.audio, music };
if (fs.existsSync(out)) fs.copyFileSync(out, out.replace(/\.rcproj$/, ".before-rebuild.rcproj"));
fs.writeFileSync(out, JSON.stringify(project, null, 2));
console.log(`wrote ${out}: ${lyricTrack.clips.length} lyric lines, ${cfg.scenes.length} scenes, ${media.length} media, ${(songLen + outro.duration).toFixed(1)} s`);
