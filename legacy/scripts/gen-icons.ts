#!/usr/bin/env bun
/**
 * Generates `outpeek-app/ui/components/icons.slint` from the icons the React
 * app ships — the real Lucide geometry, not a redrawn approximation.
 *
 *   bun scripts/gen-icons.ts            # write the file
 *   bun scripts/gen-icons.ts --check    # exit 1 when the committed file is stale
 *
 * Source of truth: `node_modules/lucide-react/dist/esm/icons/<kebab>.mjs`,
 * whose `__iconNode` is an array of `[tag, attrs]` primitives. The files are
 * read as text (not imported) so no React runtime is needed: a few modules are
 * one-line re-export shims, and those are followed to the module that owns the
 * geometry.
 *
 * Every primitive becomes SVG path commands, because Slint's `Path` takes one
 * `commands` string and no shape elements:
 *
 *   path      -> `d` verbatim (Lucide already emits compact SVG)
 *   line      -> M x1 y1 L x2 y2
 *   circle    -> two half-arcs, closed
 *   rect      -> rounded-rect path (arcs when rx/ry), sharp otherwise
 *   ellipse   -> two half-arcs with independent radii
 *   polyline  -> M + L
 *   polygon   -> M + L + Z
 *
 * Output is deterministic: icons sorted by component name, numbers trimmed to
 * six decimals with no trailing zeros. `scripts/icons-check.ts` runs the same
 * builder against the committed file in `bun run verify`.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const LUCIDE = join(ROOT, "node_modules/lucide-react/dist/esm/icons");
export const ICON_FILE = join(ROOT, "outpeek-app/ui/components/icons.slint");

/** The icon set the shipping React app imports (`src/components/**`). */
const ICONS: ReadonlyArray<{ component: string; lucide: string }> = [
  { component: "IconActivity", lucide: "activity" },
  { component: "IconAlertTriangle", lucide: "alert-triangle" },
  { component: "IconArrowLeftRight", lucide: "arrow-left-right" },
  { component: "IconCheck", lucide: "check" },
  { component: "IconChevronDown", lucide: "chevron-down" },
  { component: "IconCoffee", lucide: "coffee" },
  { component: "IconCopy", lucide: "copy" },
  { component: "IconCpu", lucide: "cpu" },
  { component: "IconCrosshair", lucide: "crosshair" },
  { component: "IconDownload", lucide: "download" },
  { component: "IconEye", lucide: "eye" },
  { component: "IconFileCode2", lucide: "file-code-2" },
  { component: "IconFlaskConical", lucide: "flask-conical" },
  { component: "IconHardDrive", lucide: "hard-drive" },
  { component: "IconMessageSquare", lucide: "message-square" },
  { component: "IconMinus", lucide: "minus" },
  { component: "IconMonitor", lucide: "monitor" },
  { component: "IconPencil", lucide: "pencil" },
  { component: "IconPlay", lucide: "play" },
  { component: "IconPlus", lucide: "plus" },
  { component: "IconRadio", lucide: "radio" },
  { component: "IconRefreshCw", lucide: "refresh-cw" },
  { component: "IconSave", lucide: "save" },
  { component: "IconSearch", lucide: "search" },
  { component: "IconSettings", lucide: "settings" },
  { component: "IconShieldCheck", lucide: "shield-check" },
  { component: "IconShoppingBag", lucide: "shopping-bag" },
  { component: "IconSquare", lucide: "square" },
  { component: "IconTerminal", lucide: "terminal" },
  { component: "IconTrash2", lucide: "trash-2" },
  { component: "IconTrendingUp", lucide: "trending-up" },
  { component: "IconUserCheck", lucide: "user-check" },
  { component: "IconUsers", lucide: "users" },
  { component: "IconWand2", lucide: "wand-2" },
  { component: "IconX", lucide: "x" },
];

type Primitive = [string, Record<string, string>];

/** Thrown when a Lucide module uses a primitive this generator cannot express. */
export class UnsupportedPrimitive extends Error {}

/**
 * Follows re-export shims (`export { default } from './x.mjs'`) to the module
 * that owns the `__iconNode` table, then parses that table as JSON.
 */
function iconNode(name: string, seen: string[] = []): { prims: Primitive[]; owner: string } {
  if (seen.includes(name)) throw new Error(`lucide: re-export cycle through "${name}"`);
  const file = join(LUCIDE, `${name}.mjs`);
  let text: string;
  try {
    text = readFileSync(file, "utf8");
  } catch {
    throw new Error(`lucide: no icon module for "${name}" (looked in ${file})`);
  }

  const start = text.indexOf("const __iconNode = [");
  if (start === -1) {
    const shim = /export\s*\{\s*default\s*\}\s*from\s*["']\.\/([^"']+)\.mjs["']/.exec(text);
    if (!shim) throw new Error(`lucide: "${name}" has neither __iconNode nor a re-export shim`);
    return iconNode(shim[1], [...seen, name]);
  }

  const open = text.indexOf("[", start);
  const close = text.lastIndexOf("];");
  const literal = text.slice(open, close + 1);
  // The tables are JavaScript object literals: bare keys, sometimes a trailing
  // comma. Quote the keys and drop trailing commas so JSON.parse can read it —
  // no `eval`, no React import, no dependency on lucide's runtime helpers.
  const json = literal
    .replace(/([{,]\s*)([A-Za-z_$][\w$]*)\s*:/g, '$1"$2":')
    .replace(/,(\s*[\]}])/g, "$1");
  const prims = JSON.parse(json) as Primitive[];
  return { prims, owner: name };
}

/** Six decimals, trailing zeros dropped, leading zero dropped (SVG style). */
function num(value: number | string): string {
  const n = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(n)) throw new Error(`icon geometry: not a number: ${String(value)}`);
  let s = n.toFixed(6).replace(/0+$/, "").replace(/\.$/, "");
  if (s === "-0") s = "0";
  if (s.startsWith("0.")) s = s.slice(1);
  else if (s.startsWith("-0.")) s = `-${s.slice(2)}`;
  return s;
}

function points(points: string, close: boolean): string {
  const coords = points.trim().split(/[\s,]+/).map(Number);
  if (coords.length < 4 || coords.length % 2 !== 0) {
    throw new UnsupportedPrimitive(`poly points: expected x,y pairs, got "${points}"`);
  }
  let out = `M ${num(coords[0])} ${num(coords[1])}`;
  for (let i = 2; i < coords.length; i += 2) out += ` L ${num(coords[i])} ${num(coords[i + 1])}`;
  return close ? `${out} Z` : out;
}

/** Two half-arcs close a circle: the same trick in both directions. */
function ellipse(cx: number, cy: number, rx: number, ry: number): string {
  return (
    `M ${num(cx - rx)} ${num(cy)}` +
    ` a ${num(rx)} ${num(ry)} 0 1 0 ${num(rx * 2)} 0` +
    ` a ${num(rx)} ${num(ry)} 0 1 0 ${num(rx * -2)} 0 Z`
  );
}

function rect(a: Record<string, string>): string {
  const x = Number(a.x);
  const y = Number(a.y);
  const w = Number(a.width);
  const h = Number(a.height);
  // SVG rule: a lone rx or ry supplies the other one.
  const rawX = a.rx !== undefined ? Number(a.rx) : a.ry !== undefined ? Number(a.ry) : 0;
  const rawY = a.ry !== undefined ? Number(a.ry) : rawX;
  const rx = Math.min(Math.max(rawX, 0), w / 2);
  const ry = Math.min(Math.max(rawY, 0), h / 2);
  if (rx === 0 || ry === 0) {
    return `M ${num(x)} ${num(y)} H ${num(x + w)} V ${num(y + h)} H ${num(x)} Z`;
  }
  return (
    `M ${num(x + rx)} ${num(y)}` +
    ` H ${num(x + w - rx)}` +
    ` A ${num(rx)} ${num(ry)} 0 0 1 ${num(x + w)} ${num(y + ry)}` +
    ` V ${num(y + h - ry)}` +
    ` A ${num(rx)} ${num(ry)} 0 0 1 ${num(x + w - rx)} ${num(y + h)}` +
    ` H ${num(x + rx)}` +
    ` A ${num(rx)} ${num(ry)} 0 0 1 ${num(x)} ${num(y + h - ry)}` +
    ` V ${num(y + ry)}` +
    ` A ${num(rx)} ${num(ry)} 0 0 1 ${num(x + rx)} ${num(y)} Z`
  );
}

function commands(prim: Primitive): string {
  const [tag, a] = prim;
  switch (tag) {
    case "path":
      return absoluteFirstMove(a.d);
    case "line":
      return `M ${num(a.x1)} ${num(a.y1)} L ${num(a.x2)} ${num(a.y2)}`;
    case "circle":
      return ellipse(Number(a.cx), Number(a.cy), Number(a.r), Number(a.r));
    case "ellipse":
      return ellipse(Number(a.cx), Number(a.cy), Number(a.rx), Number(a.ry));
    case "rect":
      return rect(a);
    case "polyline":
      return points(a.points, false);
    case "polygon":
      return points(a.points, true);
    default:
      throw new UnsupportedPrimitive(`<${tag}> has no Path equivalent in this generator`);
  }
}

/**
 * All primitives of one icon are joined into a single `commands` string, so a
 * relative moveto that started its own `<path>` would otherwise be resolved
 * against the previous subpath's end point. A leading `m` is absolute in SVG;
 * re-emit it as `M`, keeping its extra coordinate pairs as the relative
 * linetos the spec says they are.
 */
function absoluteFirstMove(d: string): string {
  const match = /^\s*m\s*([-+]?[\d.]+)[\s,]+([-+]?[\d.]+)([\s\S]*)$/.exec(d);
  if (!match) return d;
  const [, x, y, tailRaw] = match;
  const tail = tailRaw.trim();
  if (tail === "") return `M ${x} ${y}`;
  // Extra moveto pairs are relative linetos; a following command letter is not.
  return /^[-+.\d]/.test(tail) ? `M ${x} ${y} l${tail}` : `M ${x} ${y} ${tail}`;
}

const LICENSE = `// Lucide icons — ISC License. Copyright (c) 2026 Lucide Icons and Contributors.
// Permission to use, copy, modify, and/or distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
// SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION
// OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
// CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
// https://lucide.dev — geometry copied from lucide-react v1.41.0 __iconNode.`;

/** Builds the whole file as text. Deterministic: same inputs, same bytes. */
export function buildIconsFile(): string {
  const out: string[] = [];
  out.push(
    "// GENERATED FILE — do not edit by hand.",
    "// Source: the icon set the React app imports from lucide-react, converted",
    "// primitive-by-primitive into Slint `Path` commands by scripts/gen-icons.ts.",
    "//",
    "//   bun scripts/gen-icons.ts           regenerate",
    "//   bun scripts/icons-check.ts         fail when this file is stale",
    "//",
    LICENSE,
    "",
    "/// Shared geometry: Lucide's own defaults — 24-unit viewbox, 2px stroke,",
    "/// round cap/join, no fill. `size` is the rendered box; call sites may set",
    "/// `stroke` and `opacity` on the icon like any other element property.",
    "component LucideIcon inherits Path {",
    "    in-out property <length> size: 16px;",
    "",
    "    width: self.size;",
    "    height: self.size;",
    "    viewbox-width: 24;",
    "    viewbox-height: 24;",
    "    stroke-width: 2px;",
    "    stroke-line-cap: round;",
    "    stroke-line-join: round;",
    "    fill: transparent;",
    "}",
    "",
  );

  const sorted = [...ICONS].sort((a, b) => a.component.localeCompare(b.component));
  for (const { component, lucide } of sorted) {
    const { prims, owner } = iconNode(lucide);
    if (prims.length === 0) throw new UnsupportedPrimitive(`lucide: "${lucide}" has no primitives`);
    const d = prims.map(commands).join(" ");
    out.push(
      `/// lucide: ${lucide}${owner === lucide ? "" : ` (alias of ${owner})`}`,
      `export component ${component} inherits LucideIcon {`,
      `    commands: "${d}";`,
      "}",
      "",
    );
  }

  return `${out.join("\n").trimEnd()}\n`;
}

/** Sorted component names, for the check's summary line. */
export function iconNames(): string[] {
  return ICONS.map((i) => i.component).sort((a, b) => a.localeCompare(b));
}

if (import.meta.main) {
  const check = process.argv.includes("--check");
  const expected = buildIconsFile();
  const actual = (() => {
    try {
      return readFileSync(ICON_FILE, "utf8");
    } catch {
      return null;
    }
  })();

  if (check) {
    if (actual === expected) {
      console.log(`icons: ${iconNames().length} icons, up to date`);
    } else {
      console.error(
        `icons: STALE — outpeek-app/ui/components/icons.slint does not match lucide-react.\n` +
          "  fix: bun scripts/gen-icons.ts",
      );
      const committed = actual?.split("\n") ?? [];
      const fresh = expected.split("\n");
      let line = 0;
      while (line < committed.length && line < fresh.length && committed[line] === fresh[line]) line += 1;
      console.error(`  first difference at line ${line + 1}:`);
      console.error(`    committed: ${committed[line] ?? "<eof>"}`);
      console.error(`    generated: ${fresh[line] ?? "<eof>"}`);
      process.exit(1);
    }
  } else if (actual === expected) {
    console.log(`icons: ${iconNames().length} icons, already up to date`);
  } else {
    writeFileSync(ICON_FILE, expected);
    console.log(`icons: wrote ${iconNames().length} icons to ${ICON_FILE}`);
  }
}
