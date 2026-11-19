import { NavLink, Outlet } from "react-router-dom";
import { cx } from "../../lib/cx";

/** Leaves only: settings is one surface, so every tab is a normal page, never a section index. */
const SECTIONS = [
  { to: "/settings/general", label: "General" },
  { to: "/settings/appearance", label: "Appearance" },
  { to: "/settings/runtime", label: "Runtime" },
  { to: "/settings/profile", label: "Profile" },
  { to: "/settings/security", label: "Security" },
  { to: "/settings/sessions", label: "Sessions" },
] as const;

/** One tab strip for the whole surface; each tab renders its own `PageHeader`. */
export default function SettingsLayout() {
  return (
    <>
      <nav
        aria-label="Settings sections"
        className="tab-strip mb-4 flex gap-1 overflow-x-auto border-b border-line"
      >
        {SECTIONS.map((section) => (
          <NavLink
            key={section.to}
            to={section.to}
            className={({ isActive }) =>
              cx(
                "relative shrink-0 px-3 py-2 text-body transition-colors",
                isActive ? "font-medium text-accent-text" : "text-muted hover:text-fg",
              )
            }
          >
            {({ isActive }) => (
              <>
                {section.label}
                {isActive ? (
                  <span
                    aria-hidden
                    className="absolute inset-x-2 bottom-0 h-[2px] rounded-full bg-[var(--color-nav-accent)]"
                  />
                ) : null}
              </>
            )}
          </NavLink>
        ))}
      </nav>
      <Outlet />
    </>
  );
}
