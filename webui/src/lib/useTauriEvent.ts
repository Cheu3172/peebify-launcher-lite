// ------------ Backend Event Hook ------------
// Subscribes a component to a backend event and cleans up when it goes away.
import { useEffect, useRef } from "react";

export function useTauriEvent<A extends unknown[]>(
  subscribe: (cb: (...args: A) => void) => Promise<() => void>,
  handler: (...args: A) => void,
): void {
  const handlerRef = useRef(handler);

  useEffect(() => {
    handlerRef.current = handler;
  });

  useEffect(() => {
    let un: (() => void) | undefined;
    let cancelled = false;
    void subscribe((...args) => handlerRef.current(...args)).then((f) => {
      if (cancelled) f();
      else un = f;
    });
    return () => {
      cancelled = true;
      un?.();
    };
  }, [subscribe]);
}
