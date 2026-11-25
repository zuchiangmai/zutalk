import type { HostEvent } from '$proto/HostEvent';
import { createCell, type Source } from './source';

export type Route =
  | { page: 'home' }
  | { page: 'host' }
  | { page: 'live' }
  | { page: 'result'; id: string; hostEvent?: HostEvent }
  | { page: 'reader'; id: string; tab?: 'transcript' | 'notes'; take?: number; compare?: boolean; lineKey?: string }
  | { page: 'library'; month?: string | null }
  | { page: 'settings' }
  | { page: 'captions' }
  | { page: 'outbound' };

export type RouteEvent =
  | { is: 'Go'; route: Route }
  | { is: 'SessionStarted' }
  | { is: 'SessionStopped'; id: string; hostEvent?: HostEvent }
  | { is: 'RecordingRemoved'; id: string };

/** True when navigating away from the pre-session Host screen to anywhere other than Live (`P5.md` §3.6). */
export function leavingHostArm(prev: Route, next: Route): boolean {
  return prev.page === 'host' && next.page !== 'host' && next.page !== 'live';
}

/** Pure state-machine transition for top-level navigation (P3.md §3.4, P5.md §3.6). */
export function routeStep(current: Route, event: RouteEvent): Route {
  switch (event.is) {
    case 'Go':
      return event.route;
    case 'SessionStarted':
      return { page: 'live' };
    case 'SessionStopped':
      return current.page === 'live'
        ? event.hostEvent
          ? { page: 'result', id: event.id, hostEvent: event.hostEvent }
          : { page: 'result', id: event.id }
        : current;
    case 'RecordingRemoved':
      if ((current.page === 'result' || current.page === 'reader') && current.id === event.id) {
        return { page: 'home' };
      }
      return current;
  }
}

const routeCell = createCell<Route>({ page: 'home' });

export const routeSource: Source<Route> = routeCell;

let leaveHostArmHook: (() => void) | null = null;

/** Registers a bus callback invoked when leaving the pre-session Host screen for a non-Live page. */
export function setLeaveHostArmHook(hook: (() => void) | null): void {
  leaveHostArmHook = hook;
}

export function navigate(event: RouteEvent): void {
  const prev = routeCell.getSnapshot();
  const next = routeStep(prev, event);
  routeCell.set(next);
  if (leavingHostArm(prev, next)) {
    leaveHostArmHook?.();
  }
}

export function go(route: Route): void {
  navigate({ is: 'Go', route });
}

export function resetRoute(): void {
  routeCell.set({ page: 'home' });
}
