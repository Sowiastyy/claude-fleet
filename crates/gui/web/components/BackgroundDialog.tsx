import { open } from '@tauri-apps/plugin-dialog';
import { FolderOpen, X } from 'lucide-react';
import { api, errorText, type Settings } from '../api';
import { BACKGROUNDS } from '../backgrounds';

const ACCENTS = ['#E2B867', '#7FB2FF', '#59C48F', '#C9A7FF'];

interface Props {
  settings: Settings;
  custom: string | null;
  onChange: (s: Settings) => void;
  onCustom: (dataUrl: string) => void;
  onClose: () => void;
  onToast: (text: string, bad?: boolean) => void;
}

export function BackgroundDialog(p: Props) {
  const s = p.settings;
  const choices: Array<[string, string, string | null]> = [
    ...BACKGROUNDS.map((b) => [b.key, b.label, b.url] as [string, string, string]),
    ['none', 'None', null],
    ...(p.custom ? [['custom', 'Yours', p.custom] as [string, string, string]] : []),
  ];
  const pick = async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'svg'] }],
    });
    if (typeof path !== 'string') return;
    try {
      const url = await api.importBackground(path);
      p.onCustom(url);
      // The import copied the file and saved where the copy is.
      p.onChange(await api.settings());
    } catch (e) {
      p.onToast(errorText(e), true);
    }
  };
  return (
    <div className="modal-scrim" onMouseDown={(e) => e.target === e.currentTarget && p.onClose()}>
      <div className="modal" role="dialog" aria-label="Background">
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <h2>Background</h2>
          <button type="button" className="icon-btn" aria-label="Close" onClick={p.onClose}>
            <X size={17} aria-hidden />
          </button>
        </div>
        <div className="thumbs">
          {choices.map(([key, label, url]) => (
            <button key={key} type="button" className="thumb" aria-pressed={s.background === key} onClick={() => p.onChange({ ...s, background: key })}>
              <span className="thumb-img">{url && <img src={url} alt="" />}</span>
              {label}
            </button>
          ))}
        </div>
        <button type="button" className="ghost-btn" style={{ alignSelf: 'flex-start' }} onClick={pick}>
          <FolderOpen size={15} aria-hidden />
          Choose image
        </button>
        <div className="slider">
          <div className="slider-head">
            <label htmlFor="dim-new">New chat darkness</label>
            <span style={{ color: 'var(--accent)', fontWeight: 600 }}>{s.newChatDim}%</span>
          </div>
          <input id="dim-new" type="range" min={0} max={95} step={5} value={s.newChatDim} onChange={(e) => p.onChange({ ...s, newChatDim: Number(e.target.value) })} />
        </div>
        <div className="slider">
          <div className="slider-head">
            <label htmlFor="dim-chat">Conversation darkness</label>
            <span style={{ color: 'var(--accent)', fontWeight: 600 }}>{s.chatDim}%</span>
          </div>
          <input id="dim-chat" type="range" min={0} max={95} step={5} value={s.chatDim} onChange={(e) => p.onChange({ ...s, chatDim: Number(e.target.value) })} />
        </div>
        <div className="slider">
          <span>Accent</span>
          <div className="swatches">
            {ACCENTS.map((c) => (
              <button
                key={c}
                type="button"
                className="swatch"
                aria-label={c}
                aria-pressed={s.accent.toLowerCase() === c.toLowerCase()}
                style={{ background: c }}
                onClick={() => p.onChange({ ...s, accent: c })}
              />
            ))}
          </div>
        </div>
        <div style={{ display: 'flex', justifyContent: 'flex-end' }}>
          <button type="button" className="btn primary" onClick={p.onClose}>Done</button>
        </div>
      </div>
    </div>
  );
}
