import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import * as ToastPrimitive from "@radix-ui/react-toast";
import { CheckCircle, Info, WarningCircle, X } from "@phosphor-icons/react";
import { cx } from "./ui";

type Tone = "pos" | "neg" | "info";
type Toast = { id: number; tone: Tone; title: string; detail?: string };
type Api = {
  success: (title: string, detail?: string) => void;
  error: (title: string, detail?: string) => void;
  info: (title: string, detail?: string) => void;
};

const ToastContext = createContext<Api | null>(null);

/** Transient feedback built on Radix Toast; announcements and dismissal are Radix's. */
export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<Toast[]>([]);
  const next = useRef(1);

  const push = useCallback((tone: Tone, title: string, detail?: string) => {
    const id = next.current++;
    setItems((current) => [...current.slice(-3), { id, tone, title, detail }]);
  }, []);

  const dismiss = useCallback((id: number) => {
    setItems((current) => current.filter((item) => item.id !== id));
  }, []);

  const api = useMemo<Api>(
    () => ({
      success: (title, detail) => push("pos", title, detail),
      error: (title, detail) => push("neg", title, detail),
      info: (title, detail) => push("info", title, detail),
    }),
    [push],
  );

  return (
    <ToastContext.Provider value={api}>
      <ToastPrimitive.Provider swipeDirection="right" duration={5000}>
        {children}
        {items.map((item) => {
          const Icon = item.tone === "pos" ? CheckCircle : item.tone === "neg" ? WarningCircle : Info;
          return (
            <ToastPrimitive.Root
              key={item.id}
              open
              onOpenChange={(open) => {
                if (!open) dismiss(item.id);
              }}
              role={item.tone === "neg" ? "alert" : "status"}
              className={cx(
                "pointer-events-auto flex items-start gap-2.5 rounded-[10px] border px-3.5 py-3 shadow-[0_12px_32px_rgba(16,18,20,0.16)]",
                "data-[state=open]:animate-in data-[state=closed]:animate-out",
                item.tone === "pos" && "border-[var(--color-pos)]/35 bg-[var(--color-pos-tint)] text-[var(--color-pos)]",
                item.tone === "neg" && "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)]",
                item.tone === "info" && "border-line bg-surface text-fg",
              )}
            >
              <Icon size={17} aria-hidden className="mt-0.5 shrink-0" />
              <div className="min-w-0 flex-1">
                <ToastPrimitive.Title className="text-small font-semibold">{item.title}</ToastPrimitive.Title>
                {item.detail ? (
                  <ToastPrimitive.Description className="mt-0.5 break-words text-caption opacity-80">
                    {item.detail}
                  </ToastPrimitive.Description>
                ) : null}
              </div>
              <ToastPrimitive.Close asChild>
                <button type="button" aria-label="dismiss" className="shrink-0 opacity-60 hover:opacity-100">
                  <X size={13} aria-hidden />
                </button>
              </ToastPrimitive.Close>
            </ToastPrimitive.Root>
          );
        })}
        <ToastPrimitive.Viewport className="pointer-events-none fixed bottom-4 right-4 z-[80] flex w-[min(360px,calc(100vw-2rem))] flex-col gap-2 outline-none" />
      </ToastPrimitive.Provider>
    </ToastContext.Provider>
  );
}

export function useToast(): Api {
  const api = useContext(ToastContext);
  if (!api) throw new Error("useToast must be used inside ToastProvider");
  return api;
}
