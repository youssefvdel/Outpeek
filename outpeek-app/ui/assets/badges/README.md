# Badge artwork

Byte-identical copies of the art the React/Tauri app ships, so the native shell
draws the same badges instead of stand-ins. Slint embeds these files at compile
time (`@image-url`); there is no runtime assets folder to lose.

| File | Copied from | Drawn by |
| --- | --- | --- |
| `kills.png` | `src/assets/icons/kills.png` | Combat Highlights — match kills (best) |
| `firstbloods.png` | `src/assets/icons/firstbloods.png` | Combat Highlights — first kills / deaths |
| `aces.png` | `src/assets/icons/aces.png` | Combat Highlights — aces |
| `score-1k.svg` | `src/assets/badges/score-1k.svg` | Tracker Score tier 1K (score ≥ 1000) |
| `score-s.svg` | `src/assets/badges/score-s.svg` | Tracker Score tier S |
| `score-a.svg` | `src/assets/badges/score-a.svg` | Tracker Score tier A |
| `score-b.svg` | `src/assets/badges/score-b.svg` | Tracker Score tier B |
| `score-c.svg` | `src/assets/badges/score-c.svg` | Tracker Score tier C |
| `score-d.svg` | `src/assets/badges/score-d.svg` | Tracker Score tier D |

Provenance: project artwork that has shipped in this repo since commits
`c143046` ("feat(combat): use supplied icons for kills, firstbloods, and aces")
and `5a060b8` ("feat(score): add TRN tier badge assets"). No third-party
licence text accompanies the files; they are not pulled from an external icon
pack at build time.

Geometry: the PNGs are 64×64 with a full-bleed circular chip; every medallion
is a 24×24-viewBox SVG hex. Both keep their aspect ratio in Slint
(`image-fit: contain` with an explicit square).

Do not edit these copies. Change the source under `src/assets/` and copy again,
so the React app and the native shell cannot drift.
