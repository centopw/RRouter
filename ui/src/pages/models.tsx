import { useCallback, useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useSearchParams } from "react-router-dom";
import { PageHeader, TabPanel, TabStrip } from "../components/ui";
import { useLayoutDraft } from "../lib/layout-draft";
import {
  qk,
  useAliases,
  useCombos,
  useIsAdmin,
  usePresetIndex,
  usePresetLayout,
} from "../lib/queries";
import { ActiveView } from "./models/active";
import { NamesView } from "./models/names";
import { RegistryView } from "./models/registry";
import { ALL, DEFAULT_SIZE, VIEWS, type ModelView } from "./models/params";

/**
 * Routing → Models, in three addressable views.
 *
 * One page used to stack five jobs in a single scroll: the preset bar, the board, the
 * hidden list, the whole registry and the alias/combo rows. Operators could not tell which
 * table they were looking at. The views split by job, not by data:
 *
 *   active   — the preset this caller routes through: order, groups, enable, hide.
 *   registry — every model the gateway knows: filters, prices, capabilities, custom rows.
 *   names    — aliases and combos as routable names: group and /v1/models visibility.
 *
 * `?view=` carries the choice, so reload, back/forward and a shared link all land on the
 * same view; every other parameter (preset, filters, pages) is preserved when switching.
 */

const OPTIONS = [
  { id: "active", label: "Active" },
  { id: "registry", label: "Registry" },
  { id: "names", label: "Names" },
] as const satisfies ReadonlyArray<{ id: ModelView; label: string }>;

const SUBTITLES: Record<ModelView, string> = {
  active: "The preset this gateway routes through: its order, groups, and what is hidden.",
  registry: "Every model this gateway can route, with its limits, price, and capabilities.",
  names: "Aliases and combos as routable names: their group and /v1/models visibility.",
};

export default function Models() {
  const isAdmin = useIsAdmin();
  const [params, setParams] = useSearchParams();
  const client = useQueryClient();

  // Presets and the alias list are admin reads; a member only ever sees the registry.
  // A link that carries registry filters (`?q=`, `?provider=`, `?shared=1`) opens the
  // view those filters belong to instead of a board that would silently ignore them.
  const requested = params.get("view");
  const hasFilters = params.has("q") || params.has("provider") || params.has("shared");
  const view: ModelView = isAdmin
    ? (VIEWS as readonly string[]).includes(requested ?? "")
      ? (requested as ModelView)
      : hasFilters
        ? "registry"
        : "active"
    : "registry";

  /** Write parameters to the URL, dropping defaults so a clean view has a clean link. */
  const setFilters = useCallback(
    (patch: Record<string, string | null>, replace = false) => {
      const next = new URLSearchParams(params);
      for (const [key, value] of Object.entries(patch)) {
        const defaulted =
          value === null ||
          value === "" ||
          (key === "provider" && value === ALL) ||
          (key === "size" && Number(value) === DEFAULT_SIZE) ||
          (key === "page" && Number(value) === 1) ||
          (key === "ipage" && Number(value) === 1) ||
          (key === "dpage" && Number(value) === 1);
        if (defaulted) next.delete(key);
        else next.set(key, value);
      }
      setParams(next, { replace });
    },
    [params, setParams],
  );

  /** The chosen view is always spelled out, so it wins over the filter default above. */
  const selectView = (next: ModelView) => setFilters({ view: next });

  // A `?q=` link opens the registry; spell that out once, so clearing the last filter
  // does not fall back to the default view and move the operator off the table.
  useEffect(() => {
    if (isAdmin && requested === null && hasFilters) setFilters({ view: "registry" }, true);
  }, [isAdmin, requested, hasFilters, setFilters]);

  // ---- preset layout ---------------------------------------------------------------
  // Shared by ACTIVE and NAMES: the board edits the list, the names view edits the group
  // and `/v1/models` flag of one entry, and both are the same draft. Keeping it here means
  // switching views cannot drop a save that is still debouncing.
  // The registry view shows no preset and no layout, so it must not fetch them.
  const presetReads = isAdmin && view !== "registry";
  const presets = usePresetIndex(presetReads);
  // `?preset=` is the addressable layout; without it the server's active preset applies.
  const presetId =
    params.get("preset") ??
    presets.data?.active_id ??
    presets.data?.default_id ??
    presets.data?.data[0]?.id ??
    null;
  const layout = usePresetLayout(presetId, presetReads);
  const layoutDraft = useLayoutDraft(layout.data, () =>
    // Only the list: refreshing the open document would reset the saved-at indicator.
    void client.invalidateQueries({ queryKey: qk.modelPresets, exact: true }),
  );
  // Alias and combo names are routable entries of the layout; the registry never needs them.
  const namesVisible = isAdmin && view !== "registry";
  const aliases = useAliases(namesVisible);
  const combos = useCombos(namesVisible);

  return (
    <>
      <PageHeader title="Models" subtitle={SUBTITLES[view]} />
      {isAdmin ? (
        <TabStrip
          options={OPTIONS}
          value={view}
          onChange={selectView}
          ariaLabel="model view"
          className="mb-4"
        >
          <TabPanel value="active">
            <ActiveView
              isAdmin={isAdmin}
              params={params}
              setFilters={setFilters}
              presets={presets}
              presetId={presetId}
              layout={layout}
              layoutDraft={layoutDraft}
              aliases={aliases}
              combos={combos}
            />
          </TabPanel>
          <TabPanel value="registry">
            <RegistryView isAdmin={isAdmin} params={params} setFilters={setFilters} />
          </TabPanel>
          <TabPanel value="names">
            <NamesView
              isAdmin={isAdmin}
              aliases={aliases}
              combos={combos}
              groups={layoutDraft.groups}
              entries={layoutDraft.entries}
              presetId={presetId}
              onAssign={(ref, group) => layoutDraft.assignGroup(ref, group)}
              onVisible={(ref, visible) => layoutDraft.setVisible(ref, visible)}
            />
          </TabPanel>
        </TabStrip>
      ) : (
        <RegistryView isAdmin={isAdmin} params={params} setFilters={setFilters} />
      )}
    </>
  );
}
