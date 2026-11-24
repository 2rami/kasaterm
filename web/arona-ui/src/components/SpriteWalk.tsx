import { useEffect, useState } from 'react';
import { characterWalkUrl } from '@/lib/mcp';
import { useCharacterSlug } from '@/lib/sprites';
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

type WalkSheet = {
  requestUrl: string;
  imageUrl: string;
  aspect: number;
  frames: number;
  duration: number;
};

export function SpriteWalk({ character, walking, width = 72, height = 100, flip = false, facing = 'front' }: SpriteWalkProps) {
  const ref = useCharacterSlug(character);
  const requestUrl = walking && ref ? characterWalkUrl(ref.slug) : '';
  const [sheet, setSheet] = useState<WalkSheet | null>(null);

  useEffect(() => {
    if (!requestUrl) return;
    const controller = new AbortController();
    let imageUrl: string | undefined;
    fetch(requestUrl, { signal: controller.signal }).then(async (response) => {
      if (!response.ok) return;
      const frames = Number(response.headers.get('x-kasa-walk-frames'));
      const frameMs = Number(response.headers.get('x-kasa-walk-ms'));
      if (!Number.isInteger(frames) || frames < 2 || !Number.isFinite(frameMs) || frameMs <= 0) return;
      const png = await response.blob();
      if (controller.signal.aborted) return;
      const src = URL.createObjectURL(png);
      imageUrl = src;
      const image = new Image();
      image.src = src;
      await image.decode();
      if (!controller.signal.aborted && image.naturalHeight > 0) {
        setSheet({ requestUrl, imageUrl: src, aspect: image.naturalWidth / frames / image.naturalHeight, frames, duration: frames * frameMs });
      }
    }).catch(() => {});

    return () => {
      controller.abort();
      if (imageUrl) URL.revokeObjectURL(imageUrl);
      setSheet((previous) => previous?.requestUrl === requestUrl ? null : previous);
    };
  }, [requestUrl]);

  const scale = Math.min(width / 20, height / 28);
  const transform = facing === 'side' && flip ? 'scaleX(-1)' : undefined;
  let figure = <SpritePortrait character={character} scale={scale} />;
  if (requestUrl && sheet?.requestUrl === requestUrl) {
    const frameWidth = Math.min(width, height * sheet.aspect);
    const frameHeight = frameWidth / sheet.aspect;
    figure = (
      <div
        className="kasa-walk-sprite"
        role="img"
        aria-label={character}
        style={{
          width: frameWidth,
          height: frameHeight,
          backgroundImage: `url(${sheet.imageUrl})`,
          backgroundSize: `${frameWidth * sheet.frames}px ${frameHeight}px`,
          backgroundRepeat: 'no-repeat',
          imageRendering: 'pixelated',
          animationDuration: `${sheet.duration}ms`,
          animationTimingFunction: `steps(${sheet.frames})`,
          '--kasa-walk-shift': `${-frameWidth * sheet.frames}px`,
        } as React.CSSProperties}
      />
    );
  }

  return (
    <div style={{ width, height, display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
      <div style={{ transform }}>{figure}</div>
    </div>
  );
}
