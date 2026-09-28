// Key combinations as the user sees them: modifiers in the platform's
// words (symbols on a Mac) and key names from the engine's key map, which
// are W3C `code` values ("KeyL", "Digit1", "Escape").

import type { Chord, Hotkeys, Mods } from "./types";

/** Control and Alt, the default of every hotkey */
const CTRL_ALT: Mods = { ctrl: true, alt: true, shift: false, meta: false };

/** The default hotkeys, as lanroam-input's `Hotkeys::default` */
export const DEFAULT_HOTKEYS: Hotkeys = {
  pause: { ...CTRL_ALT, key: 0x29 },
  lock: { ...CTRL_ALT, key: 0x0f },
  jump: CTRL_ALT,
  step: CTRL_ALT,
};

/** Key names made shorter for a keycap */
const SHORT: Record<string, string> = {
  Escape: "Esc",
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  PrintScreen: "PrtSc",
  AudioVolumeMute: "Mute",
  AudioVolumeUp: "Vol+",
  AudioVolumeDown: "Vol−",
  MediaPlayPause: "⏯",
  MediaTrackNext: "⏭",
  MediaTrackPrevious: "⏮",
};

/** The modifiers as keycaps, in the platform's order */
export function modLabels(mods: Mods, platform: string): string[] {
  const mac = platform === "macos";
  const out: string[] = [];
  if (mods.ctrl) out.push(mac ? "⌃" : "Ctrl");
  if (mods.alt) out.push(mac ? "⌥" : "Alt");
  if (mods.shift) out.push(mac ? "⇧" : "Shift");
  if (mods.meta) out.push(mac ? "⌘" : "Win");
  return out;
}

/** A key's keycap text */
export function keyLabel(key: number, names: Record<string, string>): string {
  const name = names[key];
  if (!name) return `#${key.toString(16).toUpperCase()}`;
  if (name in SHORT) return SHORT[name];
  if (/^Key[A-Z]$/.test(name)) return name.slice(3);
  if (/^Digit\d$/.test(name)) return name.slice(5);
  if (name.startsWith("Numpad")) return `Num ${name.slice(6)}`;
  return name;
}

/** A combination as keycaps */
export function chordLabels(chord: Chord, platform: string, names: Record<string, string>): string[] {
  return [...modLabels(chord, platform), keyLabel(chord.key, names)];
}

/** Whether there is Control, Alt or Meta, which typing never needs */
export function commanding(mods: Mods): boolean {
  return mods.ctrl || mods.alt || mods.meta;
}

/** Whether `key` is a function key (F1 to F24) */
function functionKey(key: number): boolean {
  return (key >= 0x3a && key <= 0x45) || (key >= 0x68 && key <= 0x73);
}

/** Whether a combination may be a hotkey (lanroam-input's `Chord::valid_hotkey`) */
export function validHotkey(chord: Chord): boolean {
  return commanding(chord) || functionKey(chord.key);
}

/** Just the modifiers of a combination */
export function modsOf(chord: Chord): Mods {
  return { ctrl: chord.ctrl, alt: chord.alt, shift: chord.shift, meta: chord.meta };
}

/** Whether two combinations are the same */
export function sameChord(a: Chord, b: Chord): boolean {
  return a.key === b.key && sameMods(a, b);
}

/** Whether two sets of modifiers are the same */
function sameMods(a: Mods, b: Mods): boolean {
  return a.ctrl === b.ctrl && a.alt === b.alt && a.shift === b.shift && a.meta === b.meta;
}

/** Whether the hotkeys are the defaults */
export function defaultHotkeys(h: Hotkeys): boolean {
  const d = DEFAULT_HOTKEYS;
  return sameChord(h.pause, d.pause) && sameChord(h.lock, d.lock) && sameMods(h.jump, d.jump) && sameMods(h.step, d.step);
}
