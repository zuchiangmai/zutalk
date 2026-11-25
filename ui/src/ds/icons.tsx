import type { ReactNode } from 'react';

export type IconName =
  | 'lock'
  | 'plus'
  | 'search'
  | 'settings'
  | 'mic'
  | 'mic-off'
  | 'chevron-down'
  | 'chevron-right'
  | 'x'
  | 'upload'
  | 'download'
  | 'broadcast'
  | 'notes'
  | 'captions'
  | 'more'
  | 'flag'
  | 'stop'
  | 'pause'
  | 'play'
  | 'undo'
  | 'trash'
  | 'waveform'
  | 'arrow-up'
  | 'arrow-up-right'
  | 'archive'
  | 'info'
  | 'activity'
  | 'refresh'
  | 'folder'
  | 'sun'
  | 'moon'
  | 'check'
  | 'copy';

const PATHS: Record<IconName, ReactNode> = {
  lock: (
    <>
      <rect x="5" y="11" width="14" height="9" rx="2" />
      <path d="M8 11V8a4 4 0 0 1 8 0v3" />
    </>
  ),
  plus: <path d="M12 5v14M5 12h14" />,
  search: (
    <>
      <circle cx="11" cy="11" r="7" />
      <path d="m20 20-3.5-3.5" />
    </>
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2v3M12 19v3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M2 12h3M19 12h3M4.9 19.1 7 17M17 7l2.1-2.1" />
    </>
  ),
  mic: (
    <>
      <rect x="9" y="3" width="6" height="11" rx="3" />
      <path d="M5 11a7 7 0 0 0 14 0M12 18v3" />
    </>
  ),
  'mic-off': (
    <path d="M3 3l18 18M9 5a3 3 0 0 1 6 1v5M9 9v2a3 3 0 0 0 5 2.2M5 11a7 7 0 0 0 11.5 5.4M19 11a7 7 0 0 1-.5 2.6M12 18v3" />
  ),
  'chevron-down': <path d="m6 9 6 6 6-6" />,
  'chevron-right': <path d="m9 6 6 6-6 6" />,
  x: <path d="M18 6 6 18M6 6l12 12" />,
  upload: <path d="M12 20V9M7 14l5-5 5 5M5 4h14" />,
  download: <path d="M12 4v11M7 10l5 5 5-5M5 20h14" />,
  broadcast: (
    <>
      <circle cx="12" cy="12" r="2" />
      <path d="M7.8 7.8a6 6 0 0 0 0 8.4M16.2 7.8a6 6 0 0 1 0 8.4M4.9 4.9a10 10 0 0 0 0 14.2M19.1 4.9a10 10 0 0 1 0 14.2" />
    </>
  ),
  notes: <path d="M4 20h4L19 9l-4-4L4 16v4M14 6l4 4" />,
  captions: (
    <>
      <rect x="3" y="6" width="18" height="12" rx="3" />
      <path d="M7 14h4M13 14h4" />
    </>
  ),
  more: (
    <>
      <circle cx="5" cy="12" r="1.6" />
      <circle cx="12" cy="12" r="1.6" />
      <circle cx="19" cy="12" r="1.6" />
    </>
  ),
  flag: <path d="M5 21V4M5 4h11l-2 4 2 4H5" />,
  stop: <rect x="5" y="5" width="14" height="14" rx="3" />,
  pause: (
    <>
      <rect x="6" y="5" width="4" height="14" rx="1" />
      <rect x="14" y="5" width="4" height="14" rx="1" />
    </>
  ),
  play: <polygon points="7,5 19,12 7,19" />,
  undo: <path d="M9 14 4 9l5-5M4 9h10a6 6 0 0 1 0 12h-3" />,
  trash: <path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13" />,
  waveform: <path d="M4 10v4M8 6v12M12 9v6M16 5v14M20 10v4" />,
  'arrow-up': <path d="M12 19V5M6 11l6-6 6 6" />,
  'arrow-up-right': <path d="M7 17 17 7M8 7h9v9" />,
  archive: (
    <>
      <rect x="3" y="4" width="18" height="5" rx="1" />
      <path d="M5 9v10h14V9M10 13h4" />
    </>
  ),
  info: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v5M12 8h.01" />
    </>
  ),
  activity: <path d="M3 12h4l3-7 4 14 3-7h4" />,
  refresh: <path d="M21 12a9 9 0 1 1-3-6.7L21 8M21 3v5h-5" />,
  folder: <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" />,
  sun: (
    <>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
    </>
  ),
  moon: <path d="M20 14.5A8.5 8.5 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5Z" />,
  check: <path d="m5 13 4 4L19 7" />,
  copy: (
    <>
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M5 15V6a2 2 0 0 1 2-2h9" />
    </>
  ),
};

export type IconProps = {
  name: IconName;
  size?: 's' | 'm' | 'lg';
  filled?: boolean;
  label?: string;
  className?: string;
};

export function Icon({ name, size = 'm', filled = false, label, className = '' }: IconProps) {
  const cls = ['i', size === 's' ? 's' : size === 'lg' ? 'lg' : '', filled ? 'fill' : '', className]
    .filter(Boolean)
    .join(' ');
  return (
    <svg
      className={cls}
      viewBox="0 0 24 24"
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      {PATHS[name]}
    </svg>
  );
}
