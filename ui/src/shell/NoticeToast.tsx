import { useEffect } from 'react';
import {
  NOTICE_AUTO_DISMISS_MS,
  NOTICE_UNDO_DISMISS_MS,
  type NoticeItem,
} from '../bus/channel';
import { t, word } from '../bus/words';
import { Icon } from '../ds/icons';

export type NoticeToastProps = {
  notices: readonly NoticeItem[];
  onDismiss: (n: number) => void;
  onUndo: (item: NoticeItem) => void;
};

/** Displays the newest notice as a floating pill; ordinary notices dismiss in 5 s, undo in 8 s. */
export function NoticeToast({ notices, onDismiss, onUndo }: NoticeToastProps) {
  const current = notices.length > 0 ? notices[notices.length - 1] : null;

  useEffect(() => {
    if (!current) return;
    const timeoutMs = current.undo ? NOTICE_UNDO_DISMISS_MS : NOTICE_AUTO_DISMISS_MS;
    const id = setTimeout(() => onDismiss(current.n), timeoutMs);
    return () => clearTimeout(id);
  }, [current, onDismiss]);

  if (!current) return null;

  return (
    <div className="toast" role="status" aria-live="polite">
      <span>{word('code', current.code)}</span>
      {current.undo && (
        <button
          type="button"
          className="pill"
          onClick={() => {
            onUndo(current);
            onDismiss(current.n);
          }}
        >
          <Icon name="undo" size="s" />
          {t('undo')}
        </button>
      )}
      <button
        type="button"
        className="icon-btn"
        aria-label={t('close')}
        onClick={() => onDismiss(current.n)}
      >
        <Icon name="x" size="s" />
      </button>
    </div>
  );
}
