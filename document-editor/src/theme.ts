const fields = {
  background: '--paper', foreground: '--ink', muted: '--muted', border: '--line',
  surface: '--soft', surfaceHover: '--hover', surfaceActive: '--active', accent: '--accent', selection: '--selection',
  selectionForeground: '--selection-ink', checkboxForeground: '--checkbox-ink',
  danger: '--danger', success: '--success',
} as const;

/** The host owns the palette; changing colors never dispatches an editor transaction. */
export function applyTheme(value: unknown, root = document.documentElement,
  supportsColor = (color: string) => CSS.supports('color', color)) {
  const palette = value && typeof value === 'object' ? value as Record<string, unknown> : {};
  root.dataset.theme = value === 'dark' || palette.dark === true || palette.mode === 'dark' ? 'dark' : 'light';
  for (const [field, css] of Object.entries(fields)) {
    const color = palette[field];
    // Concrete CSS colors cannot smuggle declarations or depend on mutable custom properties.
    if (typeof color === 'string' && !/[;{}]|\b(?:var|url)\s*\(/i.test(color) && supportsColor(color)) root.style.setProperty(css, color);
    else root.style.removeProperty(css);
  }
}
