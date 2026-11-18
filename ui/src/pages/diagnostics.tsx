import { Badge, Dot, PageHeader, TabPanel, TabStrip } from "../components/ui";
import { useConnections, useHealth } from "../lib/queries";
import { useUrlFilterState } from "../lib/url-state";
import { Health } from "./health";
import { SystemLogs } from "./system-logs";

type DiagnosticsTab = "health" | "logs";

const TABS: ReadonlyArray<{ id: DiagnosticsTab; label: string }> = [
  { id: "health", label: "Health & Backoff" },
  { id: "logs", label: "Live System Logs" },
];

/**
 * Unified system diagnostics combining gateway health, provider backoff status,
 * and live in-memory platform logs behind a single TabStrip switcher.
 */
export function Diagnostics() {
  const health = useHealth();
  const connections = useConnections();
  const degradedCount = (connections.data ?? []).filter((c) => c.backoff_level > 0).length;

  const { values, setFilter } = useUrlFilterState({
    defaults: { tab: "health" as DiagnosticsTab },
    sanitize: {
      tab: (raw): DiagnosticsTab => (raw === "logs" ? "logs" : "health"),
    },
  });

  return (
    <>
      <PageHeader
        title="Diagnostics"
        subtitle="Gateway health, provider backoff state, and live in-memory system logs."
        actions={
          <>
            {degradedCount > 0 ? (
              <Badge tone="warn" className="tnum">
                {degradedCount} in backoff
              </Badge>
            ) : null}
            <Badge tone={health.isError ? "neg" : "pos"}>
              <Dot tone={health.isError ? "neg" : "pos"} />
              {health.isError ? "unreachable" : "serving"}
            </Badge>
          </>
        }
      />

      <TabStrip
        ariaLabel="Diagnostics views"
        options={TABS}
        value={values.tab}
        onChange={(next) => setFilter("tab", next)}
      >
        <TabPanel value="health" className="mt-4 focus:outline-none">
          <Health embedded />
        </TabPanel>
        <TabPanel value="logs" className="mt-4 focus:outline-none">
          <SystemLogs embedded />
        </TabPanel>
      </TabStrip>
    </>
  );
}

export default Diagnostics;
