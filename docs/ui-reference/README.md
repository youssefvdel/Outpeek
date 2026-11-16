# UI reference

The Tracker Network desktop client is the visual spec for every data surface.
These are the screenshots the measurements in `AGENTS.md` and in the `.slint`
files were taken from.

| File | What it pins |
| --- | --- |
| `matches-reference-1475.png` | The whole Matches surface at 1475 px wide: strip, form band, day groups, rows |
| `matches-strip-crop.png` | The top strip: season cell + three agent cells with win bars, and the recent-form cells with their bottom result bars |
| `matches-rows-crop.png` | Row anatomy: 66 px rows, 3 px result spine, 44 px portrait, day band aggregates, TRS and clutch chips, column order |

Measure with a pixel ruler, not by eye: row pitch, portrait size, type steps
and gutters come from these files. When a value here and a value in code
disagree, the screenshot wins and the code gets fixed.
