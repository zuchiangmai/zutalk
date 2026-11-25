import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react';
import type { Calendar } from '$proto/Calendar';
import type { CityEvent } from '$proto/CityEvent';
import type { HostEvent } from '$proto/HostEvent';
import type { HostState } from '$proto/HostState';
import type { State } from '$proto/State';
import {
  go,
  langName,
  langSource,
  openIdentitySheet,
  pickNearestEvent,
  say,
  send,
  sessionSource,
  stateSource,
  t,
  useQuery,
  useSource,
  when,
} from '../bus';
import { Icon } from '../ds/icons';
import { Button, Pill, Tag } from '../ds/primitives';
import { QrCode } from '../ds/QrCode';

/** Shortens an audience URL for display while keeping the host and masked prefix/fragment visible. */
export function shortenAudienceLink(url: string): string {
  const clean = url.replace(/^https?:\/\//, '');
  const hashIdx = clean.indexOf('#');
  if (hashIdx < 0) {
    return clean.length > 34 ? `${clean.slice(0, 30)}…` : clean;
  }
  const head = clean.slice(0, hashIdx);
  const frag = clean.slice(hashIdx + 1);
  const slashR = head.indexOf('/r/');
  const shortHead =
    slashR >= 0 && head.length > slashR + 7 ? `${head.slice(0, slashR + 7)}…` : head;
  const shortFrag = frag.length > 4 ? `${frag.slice(0, 4)}…` : frag;
  return `${shortHead}#${shortFrag}`;
}

function toHostEvent(cityAddress: string, ev: CityEvent): HostEvent {
  return {
    city: cityAddress,
    event: ev.path,
    title: ev.title,
    start: ev.start,
  };
}

export type HostViewProps = {
  state: State;
  calendar: Calendar | null;
  recording: boolean;
  nowMs?: number;
  onLoadCity: (address: string) => void;
  onArm: (event: HostEvent | null, captions: boolean) => void;
  onStart: () => void;
  onCancel: () => void;
  onCopyLink: (link: string) => void;
};

/** Pure props pre-session Host screen (`HostArm.dc.html`): band vs quiet paper, city event, QR, waiting viewers. */
export function HostView({
  state,
  calendar,
  recording,
  nowMs = Date.now(),
  onLoadCity,
  onArm,
  onStart,
  onCancel,
  onCopyLink,
}: HostViewProps) {
  const { host, setup } = state;
  const [cityInput, setCityInput] = useState(calendar?.address ?? setup.city ?? '');
  const [editingCity, setEditingCity] = useState(false);
  const [selectedPath, setSelectedPath] = useState<string | null>(() => {
    if (host.is === 'Armed' || host.is === 'Arming' || host.is === 'Live') {
      return host.event?.event ?? null;
    }
    return null;
  });
  const [qrExpanded, setQrExpanded] = useState(false);

  const events = calendar?.events ?? [];
  const nearestEvent = useMemo(() => pickNearestEvent(events, nowMs), [events, nowMs]);

  const activeCityEvent: CityEvent | null = useMemo(() => {
    if (events.length === 0) return null;
    if (selectedPath) {
      const found = events.find((e) => e.path === selectedPath);
      if (found) return found;
    }
    return nearestEvent;
  }, [events, selectedPath, nearestEvent]);

  const activeHostEvent: HostEvent | null = useMemo(() => {
    if (activeCityEvent && calendar) {
      return toHostEvent(calendar.address, activeCityEvent);
    }
    if (host.is === 'Armed' || host.is === 'Arming' || host.is === 'Live') {
      return host.event;
    }
    return null;
  }, [activeCityEvent, calendar, host]);

  const captionsOn =
    host.is === 'Arming'
      ? host.captions
      : host.is === 'Armed'
        ? host.captions
        : host.is === 'Live'
          ? host.outbound
          : false;

  const isOutbound = captionsOn;
  const link = host.is === 'Armed' || host.is === 'Live' ? host.link : null;
  const viewers = host.is === 'Armed' || host.is === 'Live' ? host.viewers : 0;
  const eventTitle = activeHostEvent?.title ?? null;

  useEffect(() => {
    if (!qrExpanded) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        setQrExpanded(false);
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [qrExpanded]);

  const handleCitySubmit = (e: FormEvent) => {
    e.preventDefault();
    const trimmed = cityInput.trim();
    if (!trimmed) return;
    onLoadCity(trimmed);
    setEditingCity(false);
  };

  const handleCycleEvent = () => {
    if (!calendar || events.length === 0) return;
    const idx = activeCityEvent ? events.findIndex((e) => e.path === activeCityEvent.path) : -1;
    const next = events[(idx + 1) % events.length];
    setSelectedPath(next.path);
    onArm(toHostEvent(calendar.address, next), captionsOn);
  };

  const handleToggleCaptions = () => {
    onArm(activeHostEvent, !captionsOn);
  };

  return (
    <div className="main" style={{ flex: 1 }}>
      {isOutbound ? (
        <header className="band" role="status">
          <Icon name="broadcast" />
          <b>{t('host.band.armed')}</b>
          <span className="sep" />
          <span className="chip">{t('host.audience_captions')}</span>
          {eventTitle && <span>{eventTitle}</span>}
          <span className="spacer" />
          <Pill onClick={onCancel}>{t('host.cancel')}</Pill>
        </header>
      ) : (
        <header className="top">
          <span className="private">
            <Icon name="lock" size="s" />
            {t('private.local')}
          </span>
          {eventTitle && <Tag>{t('host.can_file_after', eventTitle)}</Tag>}
          <span className="spacer" />
          <Pill quiet onClick={onCancel}>
            {t('host.cancel')}
          </Pill>
        </header>
      )}

      <section className="stage" style={{ justifyContent: 'flex-start', paddingTop: 30 }}>
        <button
          type="button"
          className="rec"
          aria-label={t('host.start')}
          disabled={recording || host.is === 'Arming'}
          onClick={onStart}
        >
          <Icon name="mic" />
        </button>
        <div className="stage-t">{t('host.start')}</div>

        <div className="sheet" style={{ marginTop: 22 }}>
          {/* Event selection */}
          <div className="sheet-row">
            <span className="k">{t('city.event')}</span>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8, minWidth: 0 }}>
              {calendar && events.length > 0 && !editingCity ? (
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <button
                    type="button"
                    className="pill wide"
                    aria-label={t('city.event')}
                    onClick={handleCycleEvent}
                  >
                    <span style={{ minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis' }}>
                      {activeCityEvent?.title ?? t('host.event_none')}
                    </span>
                    <span
                      style={{
                        display: 'inline-flex',
                        alignItems: 'center',
                        gap: 8,
                        font: '12px var(--f-ui)',
                        color: 'var(--ink3)',
                      }}
                    >
                      {calendar.address}
                      {activeCityEvent?.start ? ` · ${when(activeCityEvent.start)}` : ''}
                      <Icon name="chevron-down" size="s" />
                    </span>
                  </button>
                  <Pill quiet onClick={() => setEditingCity(true)}>
                    {t('city')}
                  </Pill>
                </div>
              ) : (
                <form className="host-city-bar" onSubmit={handleCitySubmit}>
                  <input
                    className="field"
                    type="text"
                    aria-label={t('city')}
                    placeholder={t('host.city_placeholder')}
                    value={cityInput}
                    onChange={(e) => setCityInput(e.target.value)}
                    style={{ flex: 1, minWidth: 180 }}
                  />
                  <Pill type="submit" disabled={!cityInput.trim()}>
                    {t('host.city_load')}
                  </Pill>
                  {editingCity && (
                    <Pill quiet onClick={() => setEditingCity(false)}>
                      {t('reader.cancel')}
                    </Pill>
                  )}
                </form>
              )}
              {calendar && events.length === 0 && !editingCity && (
                <span className="k">{t('city.empty')}</span>
              )}
            </div>
          </div>

          {/* Audience captions */}
          <div className="sheet-row" style={{ alignItems: 'start' }}>
            <span className="k" style={{ paddingTop: 6 }}>
              {t('host.audience')}
            </span>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 12, minWidth: 0 }}>
              <div style={{ display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: 8 }}>
                <Pill
                  active={captionsOn}
                  aria-pressed={captionsOn}
                  onClick={handleToggleCaptions}
                >
                  <Icon name="captions" size="s" />
                  {t('host.captions_toggle')}
                </Pill>
                {captionsOn && host.is === 'Arming' && <Tag>{t('host.arming')}</Tag>}
                {captionsOn && host.is === 'Armed' && (
                  <Tag>
                    <Icon name="users" size="s" />
                    <span className="num">{t('host.waiting', viewers)}</span>
                  </Tag>
                )}
              </div>

              {captionsOn && link && (
                <div className="host-qr-wrap">
                  <QrCode value={link} label={t('qr')} size={92} />
                  <div className="host-link-col">
                    <span
                      className="field mono"
                      style={{ display: 'flex', alignItems: 'center', overflow: 'hidden', whiteSpace: 'nowrap' }}
                    >
                      {shortenAudienceLink(link)}
                    </span>
                    <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
                      <Pill onClick={() => onCopyLink(link)}>
                        <Icon name="copy" size="s" />
                        {t('link.copy')}
                      </Pill>
                      <Pill onClick={() => setQrExpanded(true)}>
                        <Icon name="maximize" size="s" />
                        {t('host.qr_enlarge')}
                      </Pill>
                    </div>
                  </div>
                </div>
              )}
            </div>
          </div>

          {/* Archive note */}
          <div className="sheet-row">
            <span className="k">{t('host.archive')}</span>
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
              <Tag>
                <Icon name="archive" size="s" />
                {t('host.decide_after')}
              </Tag>
            </div>
          </div>
        </div>

        {/* Spoken / Captions language echo */}
        <div className="setup" style={{ maxWidth: 560, marginTop: 18, padding: '0 20px' }}>
          <span className="set-k">{t('setup.spoken')}</span>
          <div className="set-v">
            {setup.spoken.map((lang) => (
              <span key={lang} className="pill on">
                {langName(lang, lang)}
              </span>
            ))}
          </div>
          <span className="set-k">{t('setup.shown')}</span>
          <div className="set-v">
            {setup.shown.map((lang) => (
              <span key={lang} className="pill on">
                {langName(lang, lang)}
              </span>
            ))}
          </div>
        </div>
      </section>

      {qrExpanded && link && (
        <div
          className="scrim center"
          onClick={(e) => {
            if (e.target === e.currentTarget) setQrExpanded(false);
          }}
        >
          <div className="dialog" role="dialog" aria-modal="true" aria-label={t('qr')}>
            <button
              type="button"
              className="icon-btn dialog-x"
              aria-label={t('close')}
              onClick={() => setQrExpanded(false)}
            >
              <Icon name="x" />
            </button>
            <h2 className="dialog-t">{t('host.audience_captions')}</h2>
            {eventTitle && <p className="dialog-d">{eventTitle}</p>}
            <div className="qr-dialog-body">
              <QrCode value={link} label={t('qr')} size={240} />
              <span className="mono k">{shortenAudienceLink(link)}</span>
            </div>
            <div className="dialog-a">
              <Button variant="ghost" onClick={() => onCopyLink(link)}>
                {t('link.copy')}
              </Button>
              <Button onClick={() => setQrExpanded(false)}>{t('close')}</Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/** Bus-connected pre-session HostPage container. */
export function HostPage() {
  const state = useSource(stateSource);
  const session = useSource(sessionSource);
  useSource(langSource);
  const calendarQuery = useQuery({ view: 'Calendar' });
  const armedOnEntryRef = useRef(false);
  const loadedCityRef = useRef<string | null>(null);

  const calendar = calendarQuery.data;

  // Auto-load remembered city calendar once per city address.
  useEffect(() => {
    const remembered = state?.setup.city?.trim();
    if (!remembered || calendar || loadedCityRef.current === remembered) return;
    loadedCityRef.current = remembered;
    void send({ do: 'City', address: remembered });
  }, [state?.setup.city, calendar]);

  // Auto-arm hosting with audience captions when entering signed-in while Idle.
  useEffect(() => {
    if (!state) return;
    if (!state.me) {
      openIdentitySheet('ask');
      return;
    }
    if (!armedOnEntryRef.current && state.host.is === 'Idle') {
      armedOnEntryRef.current = true;
      const nearest = calendar ? pickNearestEvent(calendar.events, Date.now()) : null;
      const ev = nearest && calendar ? toHostEvent(calendar.address, nearest) : null;
      void send({ do: 'HostArm', event: ev, captions: true });
    }
  }, [state, calendar]);

  // Once the city calendar arrives, attach the nearest event if still armed without one.
  useEffect(() => {
    if (!state || !calendar || calendar.events.length === 0) return;
    const host: HostState = state.host;
    if (host.is === 'Armed' && host.event === null) {
      const nearest = pickNearestEvent(calendar.events, Date.now());
      if (nearest) {
        void send({
          do: 'HostArm',
          event: toHostEvent(calendar.address, nearest),
          captions: host.captions,
        });
      }
    }
  }, [state, calendar]);

  if (!state) {
    return (
      <section className="stage">
        <p className="k">{t('code.starting')}</p>
      </section>
    );
  }

  return (
    <HostView
      state={state}
      calendar={calendar}
      recording={session !== null}
      onLoadCity={(address) => {
        loadedCityRef.current = address;
        if (state.setup.city !== address) {
          void send({ do: 'Setup', setup: { ...state.setup, city: address } });
        }
        void send({ do: 'City', address });
      }}
      onArm={(event, captions) => {
        if (!state.me) {
          openIdentitySheet('ask');
          return;
        }
        void send({ do: 'HostArm', event, captions });
      }}
      onStart={() => void send({ do: 'Start' })}
      onCancel={() => {
        void send({ do: 'HostCancel' });
        go({ page: 'home' });
      }}
      onCopyLink={(link) => {
        if (globalThis.navigator?.clipboard?.writeText) {
          void globalThis.navigator.clipboard.writeText(link).then(
            () => say('link'),
            () => say('clip'),
          );
        } else {
          say('link');
        }
      }}
    />
  );
}
