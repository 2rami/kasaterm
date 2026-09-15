export type Rectangle = { left: number; top: number; right: number; bottom: number };

export function floatingPlacement(anchor: Rectangle, width: number, height: number, bounds: Rectangle, preferred: 'above' | 'below') {
  const gap = 8;
  const above = Math.max(0, anchor.top - bounds.top - gap);
  const below = Math.max(0, bounds.bottom - anchor.bottom - gap);
  const side = preferred === 'above'
    ? above >= height || above >= below ? 'above' : 'below'
    : below >= height || below >= above ? 'below' : 'above';
  const maxHeight = Math.max(32, side === 'above' ? above : below);
  const actualHeight = Math.min(height, maxHeight);
  const availableWidth = Math.max(0, bounds.right - bounds.left);
  const left = Math.max(bounds.left, Math.min((anchor.left + anchor.right - Math.min(width, availableWidth)) / 2, bounds.right - width));
  const top = side === 'above' ? anchor.top - actualHeight - gap : anchor.bottom + gap;
  return { left, top: Math.max(bounds.top, Math.min(top, bounds.bottom - actualHeight)), maxHeight, maxWidth: availableWidth, side };
}
