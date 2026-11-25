import { createCell, type Source } from './source';

export type Route =
  | { page: 'home' }
  | { page: 'live' }
  | { page: 'result'; id: string }
  | { page: 'reader'; id: string; tab?: 'transcript' | 'notes'; take?: number; compare?: boolean; lineKey?: string }
  | { page: 'library'; month?: string | null }
  | { page: 'settings' };

export type RouteEvent =
  | { is: 'Go'; route: Route }
  | { is: 'SessionStarted' }
  | { is: 'SessionStopped'; id: string }
  | { is: 'RecordingRemoved'; id: string };

/** Pure state-machine transition for top-level navigation (P3.md §3.4). */
export function routeStep(current: Route, event: RouteEvent): Route {
  switch (event.is) {
    case 'Go':
      return event.route;
    case 'SessionStarted':
      return { page: 'live' };
    case 'SessionStopped':
      return current.page === 'live' ? { page: 'result', id: event.id } : current;
    case 'RecordingRemoved':
      if ((current.page === 'result' || current.page === 'reader') && current.id === event.id) {
        return { page: 'home' };
      }
      return current;
  }
}

const routeCell = createCell<Route>({ page: 'home' });

export const routeSource: Source<Route> = routeCell;

export function navigate(event: RouteEvent): void {
  routeCell.update((prev) => routeStep(prev, event));
}

export function go(route: Route): void {
  navigate({ is: 'Go', route });
}

export function resetRoute(): void {
  routeCell.set({ page: 'home' });
}
