// Images picked, pasted or dropped into a message, made ready for Claude:
// a type the API takes, no side longer than it uses, a size it accepts.
import type { Attachment } from './api';

const TAKEN = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];
/// Claude scales anything larger down anyway; sending less is faster.
const MAX_SIDE = 2000;
/// The API refuses images over 5 MB once encoded; keep well under.
const MAX_BYTES = 3.5 * 1024 * 1024;

let seq = 0;

function readAsDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result));
    r.onerror = () => reject(r.error ?? new Error('could not read the image'));
    r.readAsDataURL(blob);
  });
}

function load(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error('not an image this window can read'));
    img.src = url;
  });
}

function split(url: string): { mediaType: string; data: string } {
  const comma = url.indexOf(',');
  const head = url.slice(5, comma);
  return { mediaType: head.split(';')[0], data: url.slice(comma + 1) };
}

export async function readImage(file: Blob, name = 'image'): Promise<Attachment> {
  if (!file.type.startsWith('image/')) throw new Error(`${name} is not an image`);
  let url = await readAsDataUrl(file);
  const img = await load(url);
  const big = Math.max(img.naturalWidth, img.naturalHeight) > MAX_SIDE;
  // A gif is left alone unless it has to shrink: redrawing it drops the
  // animation.
  if (!TAKEN.includes(file.type) || big || file.size > MAX_BYTES) {
    const scale = Math.min(1, MAX_SIDE / Math.max(img.naturalWidth, img.naturalHeight));
    const canvas = document.createElement('canvas');
    canvas.width = Math.max(1, Math.round(img.naturalWidth * scale));
    canvas.height = Math.max(1, Math.round(img.naturalHeight * scale));
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('could not resize the image');
    ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
    const png = canvas.toDataURL('image/png');
    // Photos come out smaller as jpeg; screenshots usually stay sharper and
    // small enough as png.
    url = file.type === 'image/png' && png.length * 0.75 <= MAX_BYTES ? png : canvas.toDataURL('image/jpeg', 0.88);
  }
  const { mediaType, data } = split(url);
  seq += 1;
  return { id: `a${Date.now()}-${seq}`, name, mediaType, data, url };
}

/// Every image among what was pasted or dropped.
export function imageFiles(list: DataTransferItemList | FileList | null | undefined): File[] {
  if (!list) return [];
  const out: File[] = [];
  for (const entry of Array.from(list as ArrayLike<DataTransferItem | File>)) {
    const file = entry instanceof File ? entry : entry.kind === 'file' ? entry.getAsFile() : null;
    if (file && file.type.startsWith('image/')) out.push(file);
  }
  return out;
}
