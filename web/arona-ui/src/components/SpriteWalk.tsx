import { SpritePortrait } from './SpritePortrait';

export type Facing = 'front' | 'back' | 'side';

export interface SpriteWalkProps {
  character: string;
  walking: boolean;
  width?: number;
  height?: number;
  flip?: boolean;
  facing?: Facing;
}

export function SpriteWalk({ character, walking, width = 72, height = 100, flip = false, facing = 'front' }: SpriteWalkProps) {
  const scale = Math.min(width / 20, height / 28);
  return (
    <div style={{ width, height, display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
      <div className={walking ? 'kasa-standing-walk' : undefined}>
        <div style={{ transform: facing === 'side' && flip ? 'scaleX(-1)' : undefined }}>
          <SpritePortrait character={character} scale={scale} />
        </div>
      </div>
    </div>
  );
}
