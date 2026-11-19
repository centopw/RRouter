import { Card, Field, PageHeader, Segmented } from "../../components/ui";
import { useAppearance, type Density, type Theme } from "../../lib/appearance";

/** Per-browser display preferences; every signed-in account can change them. */
export default function AppearanceSettings() {
  const { theme, density, setTheme, setDensity } = useAppearance();

  return (
    <>
      <PageHeader
        title="Appearance"
        subtitle="Color mode and display density apply to this browser only."
      />

      <Card>
        <div className="grid max-w-[720px] gap-4 px-5 py-5 md:grid-cols-2">
          <Field
            label="Color mode"
            hint="Dark mode darkens the whole dashboard. Changes apply immediately."
          >
            <Segmented<Theme>
              ariaLabel="Color mode"
              value={theme}
              onChange={setTheme}
              options={[
                { id: "light", label: "Light" },
                { id: "dark", label: "Dark" },
              ]}
            />
          </Field>

          <Field
            label="Display density"
            hint="Comfortable spaces rows out; compact fits more on screen."
          >
            <Segmented<Density>
              ariaLabel="Display density"
              value={density}
              onChange={setDensity}
              options={[
                { id: "comfortable", label: "Comfortable" },
                { id: "compact", label: "Compact" },
              ]}
            />
          </Field>
        </div>
      </Card>
    </>
  );
}
