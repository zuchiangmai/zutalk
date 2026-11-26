import type { From } from '$proto/From';
import type { Health } from '$proto/Health';
import type { RefineStatus } from '$proto/RefineStatus';
import type { Status } from '$proto/Status';
import type { Key } from '../bus/words';

function exhaustive(value: never): never {
  throw new Error(`Unhandled variant: ${JSON.stringify(value)}`);
}

export type SessionStatusPresentation = {
  tone: 'live' | 'paused' | 'warn' | 'muted';
  labelKey: Key;
  detailCode?: string;
};

/** Exhaustive mapping from core Session Status to UI tone and i18n keys (States.dc.html). */
export function sessionStatusView(status: Status): SessionStatusPresentation {
  switch (status.is) {
    case 'Recording':
      return { tone: 'live', labelKey: 'status.Recording' };
    case 'Paused':
      return { tone: 'paused', labelKey: 'status.Paused' };
    case 'AudioOnly':
      return { tone: 'warn', labelKey: 'status.AudioOnly', detailCode: status.why };
    case 'Connecting':
      return { tone: 'muted', labelKey: 'status.Connecting' };
    case 'Behind':
      return { tone: 'warn', labelKey: 'status.Behind' };
    default:
      return exhaustive(status);
  }
}

export type HealthPresentation = {
  tone: 'ok' | 'warn' | 'danger';
  labelKey: Key;
  whyCode?: string;
  canReconnect: boolean;
};

/** Exhaustive mapping from core pipeline/column Health to UI badge and reconnect affordance. */
export function healthView(health: Health): HealthPresentation {
  switch (health.is) {
    case 'Live':
      return { tone: 'ok', labelKey: 'health.Live', canReconnect: false };
    case 'Slow':
      return { tone: 'warn', labelKey: 'health.Slow', canReconnect: false };
    case 'Stalled':
      return { tone: 'warn', labelKey: 'health.Stalled', whyCode: health.why, canReconnect: false };
    case 'Dead':
      return { tone: 'danger', labelKey: 'health.Dead', whyCode: health.why, canReconnect: true };
    default:
      return exhaustive(health);
  }
}

export type RefinePresentation = {
  kind: 'draft' | 'refining' | 'refined' | 'failed';
  labelKey: Key;
  detailCode?: string;
  stepCode?: string;
  done?: number;
  of?: number;
  canRefine: boolean;
};

/** Exhaustive mapping from RefineStatus + audio availability to UI refine presentation. */
export function refineStatusView(refine: RefineStatus, hasAudio: boolean): RefinePresentation {
  switch (refine.is) {
    case 'Draft':
      return {
        kind: 'draft',
        labelKey: hasAudio ? 'refine.draft' : 'refine.no_audio',
        detailCode: refine.why ?? undefined,
        canRefine: hasAudio,
      };
    case 'Refining':
      return {
        kind: 'refining',
        labelKey: 'refine.refining',
        stepCode: refine.step,
        done: refine.done,
        of: refine.of,
        canRefine: false,
      };
    case 'Refined':
      return {
        kind: 'refined',
        labelKey: 'refine.refined',
        canRefine: false,
      };
    case 'Failed':
      return {
        kind: 'failed',
        labelKey: 'refine.failed',
        detailCode: refine.code,
        canRefine: hasAudio,
      };
    default:
      return exhaustive(refine);
  }
}

export type KeyFromPresentation = {
  labelKey: Key;
  tone: 'ok' | 'accent' | 'muted';
};

/** Exhaustive mapping from core key provenance (From) to UI label and tone. */
export function keyFromView(from: From): KeyFromPresentation {
  switch (from) {
    case 'Nowhere':
      return { labelKey: 'from.Nowhere', tone: 'muted' };
    case 'Own':
      return { labelKey: 'from.Own', tone: 'ok' };
    case 'Granted':
      return { labelKey: 'from.Granted', tone: 'accent' };
    default:
      return exhaustive(from);
  }
}
