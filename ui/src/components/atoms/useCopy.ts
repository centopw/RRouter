import { useEffect, useRef, useState } from "react";

/** Clipboard write with the pre-Clipboard-API fallback; returns a transient flag. */
export function useCopy(): [string | null, (value: string) => void] {
  const [copied, setCopied] = useState<string | null>(null);
  const timer = useRef<number | null>(null);
  useEffect(() => () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
  }, []);
  return [
    copied,
    (value: string) => {
      const done = () => {
        setCopied(value);
        if (timer.current !== null) window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setCopied(null), 1_500);
      };
      // On an insecure origin (plain http on a LAN address) `navigator.clipboard` is
      // undefined, not merely rejected. Guard the property before calling it.
      const legacy = () => {
        const area = document.createElement("textarea");
        area.value = value;
        area.setAttribute("readonly", "");
        area.style.position = "fixed";
        area.style.top = "0";
        area.style.left = "0";
        area.style.opacity = "0";
        document.body.appendChild(area);
        area.select();
        area.setSelectionRange(0, value.length);
        try {
          if (document.execCommand("copy")) done();
        } finally {
          area.remove();
        }
      };
      try {
        const write = navigator.clipboard?.writeText;
        if (write) void write.call(navigator.clipboard, value).then(done).catch(legacy);
        else legacy();
      } catch {
        legacy();
      }
    },
  ];
}
