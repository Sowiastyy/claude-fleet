// Color schemes. The tokens themselves live in styles.css under
// `:root[data-theme=…]`; this side knows their names, their default accents,
// the terminal palette each wants, and how to put a choice on the page.
// Dark, Light, Blue and Graphite follow SmartWood Console's themes.
import type { ITheme } from '@xterm/xterm';

export type ThemeKey = 'harbor' | 'dark' | 'light' | 'blue' | 'graphite';

export interface Theme {
  key: ThemeKey;
  label: string;
  light: boolean;
  /// What the preview tile shows: background, panel, text, accent.
  preview: [string, string, string, string];
  accent: string;
}

export const THEMES: Theme[] = [
  { key: 'harbor', label: 'Harbor', light: false, preview: ['#0d131d', '#18222f', '#e6ebf2', '#e2b867'], accent: '#e2b867' },
  { key: 'dark', label: 'Dark', light: false, preview: ['#0a0a0b', '#17171a', '#f1f1f2', '#e3a24a'], accent: '#e3a24a' },
  { key: 'light', label: 'Light', light: true, preview: ['#f4f4f2', '#ffffff', '#1b1b1d', '#b3781f'], accent: '#b3781f' },
  { key: 'blue', label: 'Blue', light: false, preview: ['#0b1120', '#17223a', '#f8fafc', '#3b82f6'], accent: '#3b82f6' },
  { key: 'graphite', label: 'Graphite', light: false, preview: ['#131519', '#1e2026', '#c8ccd4', '#c9a022'], accent: '#c9a022' },
];

/// Accent presets, each with a tone for dark themes and one for light.
export const ACCENTS: Array<{ key: string; label: string; dark: string; light: string }> = [
  { key: 'blue', label: 'Blue', dark: '#5f8ef0', light: '#3565c4' },
  { key: 'green', label: 'Green', dark: '#57b16f', light: '#2f7d46' },
  { key: 'teal', label: 'Teal', dark: '#3fb3c4', light: '#207f8e' },
  { key: 'purple', label: 'Purple', dark: '#a678e8', light: '#7c4fc4' },
  { key: 'pink', label: 'Pink', dark: '#e06ca8', light: '#c04a86' },
  { key: 'red', label: 'Red', dark: '#e1605f', light: '#c0403f' },
];

export const MIN_CONTRAST = 2.5;

export function theme(key: string): Theme {
  return THEMES.find((t) => t.key === key) ?? THEMES[0];
}

/// The accent a setting means under a theme: its own when empty, a preset
/// in the theme's tone, or a color as given.
export function accentColor(themeKey: string, accent: string): string {
  const t = theme(themeKey);
  if (!accent) return t.accent;
  const preset = ACCENTS.find((a) => a.key === accent);
  if (preset) return t.light ? preset.light : preset.dark;
  return /^#[0-9a-f]{6}$/i.test(accent) ? accent : t.accent;
}

function rgb(hex: string): [number, number, number] {
  const n = parseInt(hex.slice(1), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

function luminance(hex: string): number {
  const [r, g, b] = rgb(hex).map((c) => {
    const v = c / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/// Whether a color stands out enough on the theme's background.
export function readableOn(themeKey: string, color: string): boolean {
  return contrast(color, theme(themeKey).preview[0]) >= MIN_CONTRAST;
}

function onAccent(color: string): string {
  return luminance(color) > 0.45 ? '#141414' : '#ffffff';
}

const STORE = 'fleet.theme';

/// Put a theme and accent on the page, and remember them for the next start
/// so the window does not open in the wrong colors.
export function applyTheme(themeKey: string, accent: string) {
  const t = theme(themeKey);
  const root = document.documentElement;
  root.dataset.theme = t.key;
  const color = accentColor(t.key, accent);
  root.style.setProperty('--accent', color);
  root.style.setProperty('--on-accent', accent ? onAccent(color) : t.light ? '#ffffff' : 'var(--on-accent-base)');
  try {
    localStorage.setItem(STORE, JSON.stringify({ theme: t.key, accent }));
  } catch {
    // Remembering is a nicety.
  }
  window.dispatchEvent(new CustomEvent('fleet-theme'));
}

export function applyStoredTheme() {
  try {
    const saved = JSON.parse(localStorage.getItem(STORE) ?? 'null');
    if (saved && typeof saved.theme === 'string') applyTheme(saved.theme, String(saved.accent ?? ''));
  } catch {
    // Nothing stored, or nothing readable: the default stays.
  }
}

const DARK_ANSI = {
  red: '#f07c70',
  green: '#59c48f',
  yellow: '#f0b54a',
  blue: '#7fb2ff',
  magenta: '#c9a7ff',
  cyan: '#6fd3d0',
  brightRed: '#ff9b90',
  brightGreen: '#7fdcaa',
  brightYellow: '#f5cd7a',
  brightBlue: '#a3c8ff',
  brightMagenta: '#dcc4ff',
  brightCyan: '#95e3e0',
};

const LIGHT_ANSI = {
  red: '#b03a39',
  green: '#2f7d46',
  yellow: '#8a6414',
  blue: '#2f5fb0',
  magenta: '#7c4fc4',
  cyan: '#1d7a87',
  brightRed: '#c24a49',
  brightGreen: '#3f8f57',
  brightYellow: '#a07419',
  brightBlue: '#3e6ea8',
  brightMagenta: '#8f5fd6',
  brightCyan: '#207f8e',
};

/// The terminal's colors, read off the page so they follow the theme.
export function terminalTheme(): ITheme {
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  const light = theme(document.documentElement.dataset.theme ?? '').light;
  return {
    background: 'rgba(0,0,0,0)',
    foreground: v('--fog'),
    cursor: v('--accent'),
    cursorAccent: v('--ink'),
    selectionBackground: light ? 'rgba(53,101,168,0.25)' : 'rgba(127,178,255,0.35)',
    black: light ? '#1b1b1d' : v('--ink'),
    white: light ? '#55555c' : v('--fog-2'),
    brightBlack: v('--faint'),
    brightWhite: light ? '#1b1b1d' : '#ffffff',
    ...(light ? LIGHT_ANSI : DARK_ANSI),
  };
}
