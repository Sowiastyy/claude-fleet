import { useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { Check, FolderOpen, Palette, Pipette, X } from 'lucide-react';
import { api, errorText, type Settings } from '../api';
import { BACKGROUNDS } from '../backgrounds';
import { ACCENTS, THEMES, accentColor, contrast, MIN_CONTRAST, readableOn, theme } from '../themes';

interface Props {
  settings: Settings;
  custom: string | null;
  onChange: (s: Settings) => void;
  onCustom: (dataUrl: string) => void;
  onClose: () => void;
  onToast: (text: string, bad?: boolean) => void;
}

type Section = 'appearance';

export function SettingsDialog(p: Props) {
  const [section, setSection] = useState<Section>('appearance');
  const s = p.settings;
  const t = theme(s.theme);
  const [low, setLow] = useState<number | null>(null);
  const isCustom = !!s.accent && !ACCENTS.some((a) => a.key === s.accent);

  const choices: Array<[string, string, string | null]> = [
    ...BACKGROUNDS.map((b) => [b.key, b.label, b.url] as [string, string, string]),
    ['none', 'None', null],
    ...(p.custom ? [['custom', 'Yours', p.custom] as [string, string, string]] : []),
  ];

  const pickTheme = (key: string) => {
    // A color of one's own stays only while it still reads on the new
    // background.
    const accent = isCustom && !readableOn(key, s.accent) ? '' : s.accent;
    setLow(null);
    p.onChange({ ...s, theme: key, accent });
  };

  const pickColor = (color: string) => {
    if (!readableOn(s.theme, color)) {
      setLow(contrast(color, t.preview[0]));
      return;
    }
    setLow(null);
    p.onChange({ ...s, accent: color });
  };

  const pickImage = async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'svg'] }],
    });
    if (typeof path !== 'string') return;
    try {
      const url = await api.importBackground(path);
      p.onCustom(url);
      // The import copied the file and saved where the copy is.
      p.onChange({ ...(await api.settings()), theme: s.theme, accent: s.accent });
    } catch (e) {
      p.onToast(errorText(e), true);
    }
  };

  return (
    <div className="modal-scrim" onMouseDown={(e) => e.target === e.currentTarget && p.onClose()}>
      <div className="modal settings" role="dialog" aria-label="Settings">
        <div style={{ display: 'flex', alignItems: 'center' }}>
          <h2>Settings</h2>
          <button type="button" className="icon-btn" aria-label="Close" onClick={p.onClose}>
            <X size={17} aria-hidden />
          </button>
        </div>

        <div className="settings-body">
          <nav className="settings-nav" aria-label="Settings sections">
            <button type="button" aria-current={section === 'appearance'} onClick={() => setSection('appearance')}>
              <Palette size={15} aria-hidden />
              Appearance
            </button>
          </nav>

          {section === 'appearance' && (
            <div className="settings-pane" role="region" aria-label="Appearance">
              <section className="field">
                <h3>Theme</h3>
                <div className="themes">
                  {THEMES.map((th) => {
                    const [bg, panel, fg, accent] = th.preview;
                    return (
                      <button key={th.key} type="button" className="theme-tile" aria-pressed={s.theme === th.key} onClick={() => pickTheme(th.key)}>
                        <span className="theme-preview" style={{ background: bg }} aria-hidden>
                          <span className="tp-rail" style={{ background: panel }} />
                          <span className="tp-lines">
                            <span style={{ background: fg, width: '70%' }} />
                            <span style={{ background: fg, width: '45%', opacity: 0.55 }} />
                            <span className="tp-bubble" style={{ background: panel }} />
                          </span>
                          <span className="tp-dot" style={{ background: accent }} />
                        </span>
                        <span className="theme-label">
                          {th.label}
                          {s.theme === th.key && <Check size={13} strokeWidth={2.5} aria-hidden />}
                        </span>
                      </button>
                    );
                  })}
                </div>
              </section>

              <section className="field">
                <h3>Accent</h3>
                <div className="swatches">
                  <button
                    type="button"
                    className="swatch"
                    title="Theme"
                    aria-label="Theme accent"
                    aria-pressed={!s.accent}
                    style={{ background: t.accent }}
                    onClick={() => {
                      setLow(null);
                      p.onChange({ ...s, accent: '' });
                    }}
                  />
                  <span className="swatch-rule" aria-hidden />
                  {ACCENTS.map((a) => (
                    <button
                      key={a.key}
                      type="button"
                      className="swatch"
                      title={a.label}
                      aria-label={a.label}
                      aria-pressed={s.accent === a.key}
                      style={{ background: t.light ? a.light : a.dark }}
                      onClick={() => {
                        setLow(null);
                        p.onChange({ ...s, accent: a.key });
                      }}
                    />
                  ))}
                  <label
                    className={`swatch swatch-custom ${isCustom ? 'set' : ''}`}
                    title="Custom"
                    style={isCustom ? { background: accentColor(s.theme, s.accent) } : undefined}
                  >
                    <Pipette size={13} aria-hidden />
                    <span className="sr-only">Custom accent</span>
                    <input type="color" value={accentColor(s.theme, s.accent)} onChange={(e) => pickColor(e.target.value)} />
                  </label>
                  {low !== null && (
                    <span className="small warn-text" role="status">
                      Low contrast {low.toFixed(1)} &lt; {MIN_CONTRAST}
                    </span>
                  )}
                </div>
              </section>

              <section className="field">
                <h3>Background</h3>
                <div className="thumbs">
                  {choices.map(([key, label, url]) => (
                    <button key={key} type="button" className="thumb" aria-pressed={s.background === key} onClick={() => p.onChange({ ...s, background: key })}>
                      <span className="thumb-img">{url && <img src={url} alt="" />}</span>
                      {label}
                    </button>
                  ))}
                </div>
                <button type="button" className="ghost-btn" style={{ alignSelf: 'flex-start' }} onClick={pickImage}>
                  <FolderOpen size={15} aria-hidden />
                  Choose image
                </button>
              </section>

              <div className="slider">
                <div className="slider-head">
                  <label htmlFor="dim-new">New chat darkness</label>
                  <span className="slider-value">{s.newChatDim}%</span>
                </div>
                <input id="dim-new" type="range" min={0} max={95} step={5} value={s.newChatDim} onChange={(e) => p.onChange({ ...s, newChatDim: Number(e.target.value) })} />
              </div>
              <div className="slider">
                <div className="slider-head">
                  <label htmlFor="dim-chat">Conversation darkness</label>
                  <span className="slider-value">{s.chatDim}%</span>
                </div>
                <input id="dim-chat" type="range" min={0} max={95} step={5} value={s.chatDim} onChange={(e) => p.onChange({ ...s, chatDim: Number(e.target.value) })} />
              </div>
            </div>
          )}
        </div>

        <div style={{ display: 'flex', justifyContent: 'flex-end' }}>
          <button type="button" className="btn primary" onClick={p.onClose}>Done</button>
        </div>
      </div>
    </div>
  );
}
