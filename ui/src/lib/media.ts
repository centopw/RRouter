import { useEffect, useState } from "react";

/**
 * The app's phone breakpoint: Tailwind's `sm` (40rem). Below it a dense table becomes a
 * card list — one DOM, chosen by this query, so no name or control exists twice.
 */
export const PHONE = "(max-width: 639px)";

/** True while the viewport is narrower than `PHONE`. Re-renders on rotation or resize. */
export function usePhone(): boolean {
  return useMedia(PHONE);
}

/**
 * True while the operator asked the OS for less motion. Drag previews and transitions that
 * move on their own must respect it.
 */
export function useReducedMotion(): boolean {
  return useMedia("(prefers-reduced-motion: reduce)");
}

function useMedia(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const list = window.matchMedia(query);
    const update = () => setMatches(list.matches);
    update();
    list.addEventListener("change", update);
    return () => list.removeEventListener("change", update);
  }, [query]);
  return matches;
}
