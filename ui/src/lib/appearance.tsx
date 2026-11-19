import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

export type Theme = "light" | "dark";
export type Density = "compact" | "comfortable";

const THEME_KEY = "rrouter:theme";
const DENSITY_KEY = "rrouter:density";

function readInitialTheme(): Theme {
  try {
    const stored = window.localStorage.getItem(THEME_KEY);
    if (stored === "dark" || stored === "light") return stored;
  } catch {
    // Storage unavailable; fall back to document dataset or light.
  }
  return document.documentElement.dataset.theme === "dark" ? "dark" : "light";
}

function readInitialDensity(): Density {
  try {
    const stored = window.localStorage.getItem(DENSITY_KEY);
    if (stored === "compact" || stored === "comfortable") return stored;
  } catch {
    // Storage unavailable.
  }
  return document.documentElement.dataset.density === "compact" ? "compact" : "comfortable";
}

type Appearance = {
  theme: Theme;
  density: Density;
  setTheme: (theme: Theme) => void;
  setDensity: (density: Density) => void;
};

const AppearanceContext = createContext<Appearance | null>(null);

/** Theme and density are per-browser preferences: the effect writes the document
 *  dataset every consumer reads, and the choice survives a reload in localStorage. */
export function AppearanceProvider({ children }: { children: ReactNode }) {
  const [theme, setTheme] = useState<Theme>(readInitialTheme);
  const [density, setDensity] = useState<Density>(readInitialDensity);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.documentElement.classList.toggle("dark", theme === "dark");
    try {
      window.localStorage.setItem(THEME_KEY, theme);
    } catch {
      // Storage unavailable; keep in-memory choice.
    }
  }, [theme]);

  useEffect(() => {
    document.documentElement.dataset.density = density;
    try {
      window.localStorage.setItem(DENSITY_KEY, density);
    } catch {
      // Storage unavailable; keep in-memory choice.
    }
  }, [density]);

  const value = useMemo<Appearance>(
    () => ({ theme, density, setTheme, setDensity }),
    [theme, density],
  );

  return <AppearanceContext.Provider value={value}>{children}</AppearanceContext.Provider>;
}

export function useAppearance(): Appearance {
  const value = useContext(AppearanceContext);
  if (!value) throw new Error("useAppearance must be used inside AppearanceProvider");
  return value;
}
