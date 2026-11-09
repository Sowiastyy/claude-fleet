import harbor from './assets/bg/harbor.svg';
import ridge from './assets/bg/ridge.svg';
import contours from './assets/bg/contours.svg';

export const BACKGROUNDS = [
  { key: 'harbor', label: 'Harbor', url: harbor },
  { key: 'ridge', label: 'Ridge', url: ridge },
  { key: 'contours', label: 'Contours', url: contours },
];

export function backgroundUrl(key: string, custom: string | null): string | null {
  if (key === 'custom') return custom;
  return BACKGROUNDS.find((b) => b.key === key)?.url ?? null;
}
