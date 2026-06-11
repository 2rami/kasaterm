import { CSSProperties, ReactNode } from 'react';
import { AccentColorName } from '@/design/tokens';

type Variant = 'default' | 'inset' | 'active' | 'terminal' | 'dialog';

export interface PixelPanelProps {
  variant?: Variant;
  title?: string;
  accent?: AccentColorName;
  children?: ReactNode;
  style?: CSSProperties;
  className?: string;
  noPadding?: boolean;
}

const borderByVariant: Record<Variant, string> = {
  default:  'var(--cth-panel-border)',
  inset:    'var(--cth-panel-border-inset)',
  active:   'var(--cth-panel-border)',  // accent overlay added separately
  terminal: 'var(--cth-panel-border-terminal)',
  dialog:   'var(--cth-panel-border-dialog)'
};

const fillByVariant: Record<Variant, string> = {
  default:  'var(--cth-cream-100)',
  inset:    'var(--cth-cream-200)',
  active:   'var(--cth-cream-100)',
  terminal: 'var(--cth-paper-100)',
  dialog:   'var(--cth-cream-50)'
};

export function PixelPanel({
  variant = 'default',
  title,
  accent,
  children,
  style,
  className,
  noPadding = false
}: PixelPanelProps) {
  const baseStyle: CSSProperties = {
    background: fillByVariant[variant],
    boxShadow: `${borderByVariant[variant]}, 0 2px 10px rgba(21, 41, 74, 0.06)`,
    borderRadius: 12,
    padding: noPadding ? 0 : 'var(--cth-space-3)',
    position: 'relative',
    ...style
  };

  // Active variant: paint accent over the middle border slot (3px ring at 1px inset)
  if (variant === 'active' && accent) {
    baseStyle.boxShadow = `inset 0 0 0 2px var(--cth-${accent}), 0 4px 14px rgba(74, 144, 226, 0.18)`;
  }

  return (
    <div className={className} style={baseStyle}>
      {title && (
        <div
          style={{
            margin: noPadding ? 0 : '-12px -12px 12px',
            padding: '8px 12px 6px',
            borderTopLeftRadius: 12, borderTopRightRadius: 12,
            background: accent ? `var(--cth-${accent})` : 'var(--cth-cream-100)',
            color: accent ? '#fff' : 'var(--cth-ink-700)',
            fontFamily: 'var(--cth-font-ui)', fontWeight: 700,
            fontSize: 'var(--cth-text-display-md)',
            lineHeight: 'var(--cth-lh-display-md)',
            borderBottom: '1px solid var(--cth-cream-200)'
          }}
        >
          {title}
        </div>
      )}
      {children}
    </div>
  );
}
