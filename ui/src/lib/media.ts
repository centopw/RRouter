import { useEffect, useState } from "react";

/**
 * The app's phone breakpoint: Tailwind's `sm` (40rem). Below it a dense table becomes a
 * card list — one DOM, chosen by this query, so no name or control exists twice.
 */
export const PHONE = "(max-width: 639px)";

/** True while the viewport is narrower than `PHONE`. Re-renders on rotation or resize. */
export function usePhone(): boolean {
  const [phone, setPhone] = useState(() => window.matchMedia(PHONE).matches);
  useEffect(() => {
    const query = window.matchMedia(PHONE);
    const update = () => setPhone(query.matches);
    update();
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);
  return phone;
}
