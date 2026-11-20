# Bond Money v2: the worked example

130.4 BPM: beat 0.46 s, bar 1.84 s. Every cut was on a bar line (2 bars = 3.69 s per cut). All cuts were hard cuts before this pass.

| Time | From → to | Transition | Beats |
|---|---|---|---|
| 0:00 | (start, the ReadyRock intro overlay) | fade_in 0.70 s, ending on beat 1 | – |
| 0:44.52 | church → diner entrance | dip_black | 2 |
| 1:17.69 | fridge invoice → theatre flashback | dip_white, plus a 1-beat fade_in on the reshoot overlay above it | 2 |
| 1:25.08 | savings b-roll → kitchen-dance flashback | dip_white | 2 |
| 1:43.49 | first hook downbeat | zoom_in | 1 |
| 1:58.21 | rain performance → rent-office flashback | dissolve | 2 |
| 2:09.27 | into Verse 2 ("Remember that time?") | dissolve | 2 |
| 2:39.17 | → church choir lip-sync performance (started 0.19 s after a beat; snapped to the next beat by trimming its head, as it had no footage before) | dip_white | 2 |
| 2:49.76 | choir → kitchen close-up | dip_black | 2 |
| 3:04.46 | → Hawaii flashback | dip_white | 2 |
| 3:11.80 | kitchen flashback → jail cell | hard cut (contrast) | – |
| 3:30.19 | into the bridge | dip_black | 2 |
| 3:51.0 / 4:01.36 | redemption performance overlay | fade_in to the next beat / fade_out from a beat | – |
| 4:28.91 | into the outro | dip_black | 2 |
| 4:39.94, 4:43.59, 4:47.28 | theatre → Hawaii → kitchen flashback run | dissolve each | 2 |
| 5:05.76 | jail cell → campus sunrise (final shot) | dip_white | 4 |
| 5:16.80 | story → vlog end card | dip_black, starting on the song's last beat | 4 |

Lessons:
- The first pass read the beat offset's sign backwards and moved the choir cut 0.19 s later instead of to the next beat (ending 81 ms off). `transitions.py check` caught it, and the fix was to trim a further 0.08 s. The offset is beat − cut.
- A 1 ms gap left by an earlier edit made one `set_transition` fail ("no clip directly before"); extending the earlier clip by 1 ms fixed it.
- The old MCP server dropped `scene_todos` on every save; restored from the backup each time.
- Two things worth raising with the user: the bail-office shot after the campus sunrise undercut the ending, and the title card sat on top of the ReadyRock intro at 0:00.
