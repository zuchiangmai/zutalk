import { useEffect, useState, type ButtonHTMLAttributes, type ReactNode } from 'react';

/** Armed inline confirmation resets to idle after 4 s to prevent accidental destructive taps (P3.md §3.4). */
export const CONFIRM_ARM_TIMEOUT_MS = 4_000;

export type ConfirmState = { is: 'idle' } | { is: 'armed'; since: number };

export type ConfirmEvent = { is: 'Press' } | { is: 'Timeout' } | { is: 'Cancel' };

export type ConfirmTransition = {
  next: ConfirmState;
  confirmed: boolean;
};

/** Pure state-machine transition for press-again-in-place confirmation (States.dc.html). */
export function confirmStep(state: ConfirmState, event: ConfirmEvent, now: number): ConfirmTransition {
  if (event.is === 'Timeout' || event.is === 'Cancel') {
    return { next: { is: 'idle' }, confirmed: false };
  }
  // event.is === 'Press'
  if (state.is === 'idle') {
    return { next: { is: 'armed', since: now }, confirmed: false };
  }
  if (now - state.since >= CONFIRM_ARM_TIMEOUT_MS) {
    return { next: { is: 'armed', since: now }, confirmed: false };
  }
  return { next: { is: 'idle' }, confirmed: true };
}

export type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: 'primary' | 'ghost';
  size?: 'sm' | 'md' | 'lg';
};

export function Button({ variant = 'primary', size = 'md', className = '', type = 'button', ...rest }: ButtonProps) {
  const cls = [
    'btn',
    variant === 'ghost' ? 'ghost' : '',
    size === 'sm' ? 'sm' : size === 'lg' ? 'lg' : '',
    className,
  ]
    .filter(Boolean)
    .join(' ');
  return <button type={type} className={cls} {...rest} />;
}

export type PillProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  active?: boolean;
  quiet?: boolean;
  danger?: boolean;
  add?: boolean;
  size?: 'md' | 'lg';
};

export function Pill({
  active = false,
  quiet = false,
  danger = false,
  add = false,
  size = 'md',
  className = '',
  type = 'button',
  ...rest
}: PillProps) {
  const cls = [
    'pill',
    active ? 'on' : '',
    quiet ? 'quiet' : '',
    danger ? 'danger' : '',
    add ? 'add' : '',
    size === 'lg' ? 'lg' : '',
    className,
  ]
    .filter(Boolean)
    .join(' ');
  return <button type={type} className={cls} {...rest} />;
}

export type SegOption<T extends string> = { value: T; label: string };

export type SegProps<T extends string> = {
  options: readonly SegOption<T>[];
  value: T;
  onChange: (value: T) => void;
  ariaLabel?: string;
};

export function Seg<T extends string>({ options, value, onChange, ariaLabel }: SegProps<T>) {
  return (
    <span className="seg" role="group" aria-label={ariaLabel}>
      {options.map((opt) => (
        <button
          key={opt.value}
          type="button"
          className={`seg-i${opt.value === value ? ' on' : ''}`}
          aria-pressed={opt.value === value}
          onClick={() => onChange(opt.value)}
        >
          {opt.label}
        </button>
      ))}
    </span>
  );
}

export type TagTone = 'neutral' | 'out' | 'warn' | 'danger' | 'ok';

export type TagProps = {
  tone?: TagTone;
  children: ReactNode;
  className?: string;
  title?: string;
};

export function Tag({ tone = 'neutral', children, className = '', title }: TagProps) {
  const cls = ['tag', tone === 'neutral' ? '' : tone, className].filter(Boolean).join(' ');
  return (
    <span className={cls} title={title}>
      {children}
    </span>
  );
}

export type CardProps = {
  children: ReactNode;
  className?: string;
};

export function Card({ children, className = '' }: CardProps) {
  return <div className={['card', className].filter(Boolean).join(' ')}>{children}</div>;
}

export type InlineConfirmProps = {
  label: string;
  confirmLabel: string;
  onConfirm: () => void;
  disabled?: boolean;
  now?: () => number;
};

/** Press-again-in-place irreversible action button (States.dc.html "销毁音频 · 原处再按一次"). */
export function InlineConfirm({
  label,
  confirmLabel,
  onConfirm,
  disabled = false,
  now = Date.now,
}: InlineConfirmProps) {
  const [state, setState] = useState<ConfirmState>({ is: 'idle' });

  useEffect(() => {
    if (state.is !== 'armed') return;
    const id = setTimeout(() => {
      setState((prev) => confirmStep(prev, { is: 'Timeout' }, now()).next);
    }, CONFIRM_ARM_TIMEOUT_MS);
    return () => clearTimeout(id);
  }, [state, now]);

  const handlePress = () => {
    const { next, confirmed } = confirmStep(state, { is: 'Press' }, now());
    setState(next);
    if (confirmed) onConfirm();
  };

  const handleBlur = () => {
    if (state.is === 'armed') {
      setState((prev) => confirmStep(prev, { is: 'Cancel' }, now()).next);
    }
  };

  const armed = state.is === 'armed';
  return (
    <button
      type="button"
      className={`pill ${armed ? 'danger-armed' : 'danger'}`}
      disabled={disabled}
      onClick={handlePress}
      onBlur={handleBlur}
    >
      {armed ? confirmLabel : label}
    </button>
  );
}
