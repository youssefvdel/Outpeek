// The Slint app draws the React app's Lucide icons from one generated file.
// Nothing regenerates it at build time, so a `lucide-react` bump or a new icon
// would drift silently: this check rebuilds it in memory and fails on a diff.
//
//   bun scripts/icons-check.ts
//
// No renderer, no Rust: text in, text out.
export {};

import { readFileSync } from "node:fs";

import { ICON_FILE, buildIconsFile, iconNames } from "./gen-icons.ts";

let failures = 0;
function check(name: string, actual: unknown, expected: unknown): void {
  const ok = actual === expected;
  if (ok) {
    console.log(`ok - ${name}`);
  } else {
    failures++;
    console.error(`FAIL - ${name}\n  expected: ${JSON.stringify(expected)}\n  actual:   ${JSON.stringify(actual)}`);
  }
}

const names = iconNames();
check("icon set is non-empty", names.length > 0, true);

let committed: string | null = null;
try {
  committed = readFileSync(ICON_FILE, "utf8");
} catch {
  committed = null;
}

check("icons.slint exists", committed !== null, true);

if (committed !== null) {
  const generated = buildIconsFile();
  check("icons.slint matches lucide-react (regenerate: bun scripts/gen-icons.ts)", committed === generated, true);

  // The header is the licence obligation, not decoration.
  check("ISC attribution header present", committed.includes("ISC License"), true);
  check("every icon carries its lucide name", (committed.match(/^\/\/\/ lucide: /gm) ?? []).length, names.length);
  for (const name of names) {
    check(`exports ${name}`, committed.includes(`export component ${name} inherits LucideIcon {`), true);
  }
}

if (failures > 0) {
  console.error(`\nicons-check: ${failures} failure(s)`);
  process.exit(1);
}
console.log(`icons-check: ${names.length} icons, no drift`);
