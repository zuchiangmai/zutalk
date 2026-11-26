import { useSyncExternalStore } from 'react';

export type Listener = () => void;

/** Read-only external store source following the dsh subscription contract. */
export type Source<T> = {
  getSnapshot(): T;
  subscribe(listener: Listener): () => void;
};

/** Mutable cell exposing a Source interface; notifies listeners only on Object.is change. */
export type Cell<T> = Source<T> & {
  set(next: T): void;
  update(fn: (prev: T) => T): void;
};

export function createCell<T>(initial: T): Cell<T> {
  let value = initial;
  const listeners = new Set<Listener>();
  return {
    getSnapshot: () => value,
    subscribe(listener: Listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    set(next: T) {
      if (Object.is(value, next)) return;
      value = next;
      for (const listener of [...listeners]) listener();
    },
    update(fn: (prev: T) => T) {
      const next = fn(value);
      if (Object.is(value, next)) return;
      value = next;
      for (const listener of [...listeners]) listener();
    },
  };
}

/** The only place in the UI allowed to call useSyncExternalStore (DECISIONS.md #1). */
export function useSource<T>(source: Source<T>): T {
  return useSyncExternalStore(source.subscribe, source.getSnapshot, source.getSnapshot);
}
