import { type Dispatch, type SetStateAction, useRef } from "react";

export type ExternalStore<T> = {
  getSnapshot: () => T;
  set: Dispatch<SetStateAction<T>>;
  subscribe: (listener: () => void) => () => void;
};

// Minimal store for useSyncExternalStore: lets a parent update a slice of
// state without re-rendering itself, only the subscribed leaf components.
export function createExternalStore<T>(initial: T): ExternalStore<T> {
  let value = initial;
  const listeners = new Set<() => void>();
  return {
    getSnapshot: () => value,
    set: (update) => {
      const next =
        typeof update === "function"
          ? (update as (current: T) => T)(value)
          : update;
      if (Object.is(next, value)) return;
      value = next;
      listeners.forEach((listener) => listener());
    },
    subscribe: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

export function useExternalStore<T>(create: () => ExternalStore<T>): ExternalStore<T> {
  const ref = useRef<ExternalStore<T> | null>(null);
  ref.current ??= create();
  return ref.current;
}
