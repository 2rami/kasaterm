import { useEffect, useState } from 'react';
import { fetchCharacters, fetchThemeRoster, fetchThemesList, characterPool } from './mcp';
import type { Agent } from '../store';

export interface CharacterRef {
  slug: string;
  /** 활성 테마가 아닌 곳에서 왔으면 그 테마 id — /character-face 에 같이 넘겨야 한다. */
  theme?: string;
}

let refs: Record<string, CharacterRef> | null = null;
let deepDone = false;
let shallowPending: Promise<void> | null = null;
let deepPending: Promise<void> | null = null;
const subs = new Set<() => void>();
const notify = () => subs.forEach((f) => f());

function harvest(c: Awaited<ReturnType<typeof fetchCharacters>>, theme?: string) {
  const into = (refs ??= {});
  for (const m of characterPool(c)) {
    // 활성 로스터가 먼저 이긴다 — 같은 이름이 두 테마에 있으면 지금 쓰는 쪽이 정본이다.
    if (m.slug && !into[m.name]) into[m.name] = theme ? { slug: m.slug, theme } : { slug: m.slug };
  }
}

async function loadDeep() {
  const meta = await fetchThemesList();
  const ids = meta.themes.map((t) => t.id).filter((id) => id && id !== meta.active);
  const rosters = await Promise.all(ids.map((id) => fetchThemeRoster(id).catch(() => null)));
  rosters.forEach((r, i) => harvest(r, ids[i]));
  deepDone = true;
}

/** Resolve the active roster first; fetch installed themes only for an unresolved name. */
export function useCharacterSlug(name: string): CharacterRef | undefined {
  const [v, bump] = useState(0);
  const found = refs?.[name];
  useEffect(() => {
    if (found || !name) return;
    const fn = () => bump((n) => n + 1);
    subs.add(fn);
    if (!refs) {
      // 빈 표로 굳히지 않는다 — 실패해도 다음 이름이 다시 시도할 수 있어야 한다.
      shallowPending ??= fetchCharacters()
        .then((c) => harvest(c))
        .catch(() => { refs ??= {}; })
        .finally(notify);
    } else if (!deepDone) {
      deepPending ??= loadDeep().catch(() => { deepDone = true; }).finally(notify);
    }
    return () => { subs.delete(fn); };
  }, [name, found, v]);
  return found;
}

export function characterNameForSlug(slug: string): string | undefined {
  return Object.entries(refs ?? {}).find(([, ref]) => ref.slug === slug)?.[0];
}

export function characterInText(...texts: (string | undefined)[]): string | null {
  const names = Object.keys(refs ?? {});
  for (const text of texts) {
    const name = names.find((candidate) => text?.includes(candidate));
    if (name) return name;
  }
  return null;
}

 // The agent record supplies identity even when its artwork arrives later.
export function assignSprites(agents: Agent[]): Agent[] {
  return agents.map((a) => ({ ...a, spriteChar: a.character }));
}
