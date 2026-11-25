import { useCallback, useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { OAuthDialog } from "../components/oauth-dialog";
import {
  AlertBanner,
  Button,
  ConfirmDialog,
  ErrorNote,
  PageHeader,
  Select,
} from "../components/ui";
import { type Connection, type OAuthProvider } from "../lib/api";
import { apiCode } from "../lib/auth";
import { refusalView, reenableConsequence } from "../lib/refusal";
import {
  useAccountQuotas,
  useAccountSubscriptions,
  useConnections,
  useIsAdmin,
  useModelProviders,
  useOAuthProviders,
  useTeams,
  useUsageSummaryByConnection,
} from "../lib/queries";
import { ProviderInventory, ProvidersKpiRow } from "./providers/account-row";
import {
  AddProviderFlow,
  EmptyProviders,
  EmptyTeamProviders,
  ProviderCodesModal,
} from "./providers/add-provider-flow";
import { ConnectionDrawer } from "./providers/connection-drawer";
import {
  buildProviderGroups,
  buildTrafficByConnection,
  filterProviderGroups,
  summarizeProviders,
  useConnectionMutations,
  type ProviderFilter,
  type ProviderNotice,
} from "./providers/types";

export default function Providers() {
  const isAdmin = useIsAdmin();
  const [params, setParams] = useSearchParams();

  // `?team=` (or legacy `?team_id=`) narrows the list inside the caller's own scope; the API
  // decides which teams those are, so a foreign id is refused rather than silently widened.
  const teamFilter = params.get("team") ?? params.get("team_id") ?? undefined;
  const inspectId = params.get("inspect");

  const connections = useConnections(true, teamFilter);
  const providers = useModelProviders();
  // OAuth sign-in, subscription plans and operator quotas are admin surfaces; a reader
  // must not fire requests that would only answer 403.
  const oauth = useOAuthProviders(isAdmin);
  const teams = useTeams();
  const quotas = useAccountQuotas(isAdmin);
  const subscriptions = useAccountSubscriptions(isAdmin);
  const usage24h = useUsageSummaryByConnection(24, true, teamFilter);
  // Both limit sources answered: only then may an account without bars read "no cap set".
  const quotaLoaded = isAdmin && quotas.isSuccess && subscriptions.isSuccess;

  const [filter, setFilter] = useState<ProviderFilter>("connected");
  const [adding, setAdding] = useState(false);
  const [initialCustomProtocol, setInitialCustomProtocol] = useState<
    "openai" | "anthropic" | null
  >(null);
  /** `/providers?add=<provider-id>` opens the credential step on that provider. */
  const [initialProvider, setInitialProvider] = useState<string | null>(null);
  const [signIn, setSignIn] = useState<OAuthProvider | null>(null);
  const [notice, setNotice] = useState<ProviderNotice | null>(null);
  const [codesOpen, setCodesOpen] = useState(false);
  const [removing, setRemoving] = useState<Connection | null>(null);
  /** The refused account whose re-enable is waiting on its consequence being read. */
  const [reenabling, setReenabling] = useState<Connection | null>(null);

  const { invalidate, refreshRegistrySoon, remove, toggle, test, testsById, recordTest } =
    useConnectionMutations(setNotice);
  const testingId = test.isPending ? (test.variables ?? null) : null;

  const accounts = useMemo(() => connections.data ?? [], [connections.data]);
  const teamList = teams.data ?? [];

  const requestToggle = (connection: Connection) => {
    if (!connection.is_active && refusalView(connection.refusal)) {
      toggle.reset();
      setReenabling(connection);
      return;
    }
    toggle.mutate({ id: connection.id, active: !connection.is_active });
  };

  const displayOf = useCallback(
    (id: string) => providers.data?.find((p) => p.id === id)?.display ?? id,
    [providers.data],
  );

  const teamName = useCallback(
    (id: string | null): string | null =>
      id === null
        ? null
        : (teams.data?.find((team) => team.id === id)?.name ?? id.slice(0, 8)),
    [teams.data],
  );

  const groups = useMemo(
    () => buildProviderGroups(accounts, providers.data, quotas.data, subscriptions.data),
    [accounts, providers.data, quotas.data, subscriptions.data],
  );

  const filteredGroups = useMemo(() => filterProviderGroups(groups, filter), [groups, filter]);

  const trafficById = useMemo(
    () => buildTrafficByConnection(usage24h.data),
    [usage24h.data],
  );

  const summary = useMemo(
    () =>
      summarizeProviders(
        accounts,
        providers.data,
        quotas.data,
        subscriptions.data,
        usage24h.data,
      ),
    [accounts, providers.data, quotas.data, subscriptions.data, usage24h.data],
  );

  const quotaById = useMemo(
    () => new Map((quotas.data ?? []).map((row) => [row.connection_id, row])),
    [quotas.data],
  );
  const subscriptionById = useMemo(
    () => new Map((subscriptions.data ?? []).map((row) => [row.connection_id, row])),
    [subscriptions.data],
  );

  const setTeamFilter = useCallback(
    (team: string) => {
      const next = new URLSearchParams(params);
      next.delete("team_id");
      if (team) next.set("team", team);
      else next.delete("team");
      setParams(next, { replace: true });
    },
    [params, setParams],
  );

  /** `Configure` on a card, a row menu or a click opens the one drawer. */
  const openConfigure = useCallback(
    (connection: Connection) => {
      if (!isAdmin) return;
      const next = new URLSearchParams(params);
      next.delete("add");
      next.delete("protocol");
      next.set("inspect", connection.id);
      setParams(next, { replace: false });
    },
    [isAdmin, params, setParams],
  );

  const providerCounts = useMemo(() => {
    let healthy = 0;
    let attention = 0;
    let disabled = 0;
    for (const group of groups) {
      if (group.state === "healthy") healthy += 1;
      else if (group.state === "disabled") disabled += 1;
      else attention += 1;
    }
    return { healthy, attention, disabled, connected: groups.length };
  }, [groups]);

  const inspectHrefFor = useCallback(
    (connectionId: string) => {
      const next = new URLSearchParams(params);
      next.set("inspect", connectionId);
      return `?${next.toString()}`;
    },
    [params],
  );

  /** Closing either surface drops both keys, so the other deep link cannot re-open behind it. */
  const closeConfigure = useCallback(() => {
    if (!params.has("inspect") && !params.has("add")) return;
    const next = new URLSearchParams(params);
    next.delete("inspect");
    next.delete("add");
    next.delete("protocol");
    setParams(next, { replace: true });
  }, [params, setParams]);

  const inspected = useMemo(
    () => (inspectId ? (accounts.find((account) => account.id === inspectId) ?? null) : null),
    [inspectId, accounts],
  );

  const reenablingView = reenabling ? refusalView(reenabling.refusal) : null;

  useEffect(() => {
    if (!inspectId || !connections.isSuccess) return;
    if (accounts.some((account) => account.id === inspectId)) return;
    setNotice({ tone: "neg", text: `Connection ${inspectId} was not found in this view.` });
    const next = new URLSearchParams(params);
    next.delete("inspect");
    setParams(next, { replace: true });
  }, [inspectId, connections.isSuccess, accounts, params, setParams]);

  const openAdd = () => {
    setInitialCustomProtocol(null);
    setInitialProvider(null);
    setAdding(true);
    setNotice(null);
  };

  // Deep links land here as `?add=custom&protocol=openai|anthropic`; the older
  // `?custom=openai|anthropic` spelling stays accepted.
  useEffect(() => {
    const legacy = params.get("custom");
    const custom = params.get("add") === "custom" ? params.get("protocol") : legacy;
    if (custom !== "openai" && custom !== "anthropic") return;
    if (isAdmin) {
      setInitialCustomProtocol(custom);
      setInitialProvider(null);
      setAdding(true);
      setNotice(null);
    }
    const next = new URLSearchParams(params);
    next.delete("custom");
    next.delete("add");
    next.delete("protocol");
    setParams(next, { replace: true });
  }, [params, setParams, isAdmin]);

  // `?add=<provider-id>` opens the add flow straight on that provider's key form, or the
  // sign-in dialog when the provider only has an OAuth flow.
  useEffect(() => {
    const requested = params.get("add");
    if (!requested || requested === "custom") return;
    if (!providers.data || !oauth.isSuccess) return;
    const found = providers.data.find((provider) => provider.id === requested);
    const signedIn =
      isAdmin && found?.kind === "oauth"
        ? oauth.data.find((row) => row.id === requested)
        : undefined;
    if (signedIn) {
      setSignIn(signedIn);
      setAdding(false);
      setNotice(null);
    } else if (isAdmin && found && found.kind !== "oauth") {
      setInitialCustomProtocol(null);
      setInitialProvider(requested);
      setAdding(true);
      setNotice(null);
    } else if (isAdmin) {
      setNotice({
        tone: "neg",
        text: found
          ? `Provider ${requested} signs in with OAuth — connect it from the provider list.`
          : `Provider ${requested} is not available in this build.`,
      });
    }
    const next = new URLSearchParams(params);
    next.delete("add");
    setParams(next, { replace: true });
  }, [params, setParams, isAdmin, providers.data, oauth.isSuccess, oauth.data]);

  const handleOAuthConnected = useCallback(
    (label: string, warning: string | null) => {
      setAdding(false);
      setSignIn(null);
      invalidate();
      refreshRegistrySoon();
      setNotice({
        tone: warning ? "neg" : "pos",
        text: warning ? `${label} connected, with a warning: ${warning}` : `${label} connected`,
      });
    },
    [invalidate, refreshRegistrySoon],
  );

  const handleAccountCreated = useCallback(() => {
    invalidate();
    refreshRegistrySoon();
  }, [invalidate, refreshRegistrySoon]);

  const keyProviders = (providers.data ?? []).filter((p) => p.kind !== "oauth");
  const subscriptionProviders = (oauth.data ?? []).map((p) => ({
    ...p,
    display: providers.data?.find((x) => x.id === p.id)?.display ?? p.display,
  }));

  return (
    <>
      <PageHeader
        title="Providers"
        subtitle="Upstream accounts this gateway routes through."
        actions={
          <>
            {teamList.length > 0 ? (
              <div className="w-[180px]">
                <Select
                  ariaLabel="filter accounts by owner team"
                  value={teamFilter ?? ""}
                  onChange={setTeamFilter}
                >
                  <option value="">{isAdmin ? "All accounts" : "My teams"}</option>
                  {teamList.map((team) => (
                    <option key={team.id} value={team.id}>
                      {team.name}
                    </option>
                  ))}
                </Select>
              </div>
            ) : null}
            {isAdmin ? (
              <Button variant="primary" onClick={openAdd}>
                Add provider
              </Button>
            ) : null}
          </>
        }
      />

      {!isAdmin ? (
        <AlertBanner tone="info" title="Read-only view" className="mb-4">
          These are the accounts your team can route through. An administrator manages credentials
          and limits.
        </AlertBanner>
      ) : null}

      <ProviderCodesModal
        open={isAdmin && codesOpen}
        providers={providers.data ?? []}
        onClose={() => setCodesOpen(false)}
      />

      <ConfirmDialog
        open={removing !== null}
        title="Remove provider account"
        message={
          <>
            Remove <span className="font-medium">{removing?.name ?? removing?.provider}</span>?
            Requests routed only here fall back or fail, and its stored credential is deleted.
          </>
        }
        confirmLabel="Remove account"
        busy={remove.isPending}
        error={remove.error}
        onConfirm={() => {
          if (removing) remove.mutate(removing.id, { onSuccess: () => setRemoving(null) });
        }}
        onClose={() => {
          if (!remove.isPending) {
            remove.reset();
            setRemoving(null);
          }
        }}
      />

      <ConfirmDialog
        open={reenabling !== null}
        title={reenablingView?.title ?? "Re-enable account"}
        message={
          reenablingView ? (
            <>
              <p className="font-medium text-fg">
                {reenabling?.name ?? reenabling?.provider}
              </p>
              <p className="mt-1">{reenablingView.description}</p>
              <p className="mt-2">{reenablingView.fix}</p>
              <p className="mt-2 font-medium">{reenableConsequence(reenablingView)}</p>
            </>
          ) : null
        }
        confirmLabel="Re-enable anyway"
        busy={toggle.isPending}
        error={toggle.error}
        onConfirm={() => {
          if (!reenabling) return;
          toggle.mutate(
            { id: reenabling.id, active: true },
            { onSuccess: () => setReenabling(null) },
          );
        }}
        onClose={() => {
          if (!toggle.isPending) {
            toggle.reset();
            setReenabling(null);
          }
        }}
      />

      {notice ? (
        <AlertBanner
          tone={notice.tone}
          title={notice.tone === "pos" ? "Account updated" : "Action failed"}
          onDismiss={() => setNotice(null)}
          className="mb-4"
        >
          <span className="break-words tnum">{notice.text}</span>
        </AlertBanner>
      ) : null}

      {connections.isError ? (
        <div className="mb-4">
          {apiCode(connections.error) === "team_forbidden" ? (
            <AlertBanner
              tone="warn"
              title="Team accounts unavailable"
              className="mb-0"
              actions={
                <Button variant="quiet" size="sm" onClick={() => setTeamFilter("")}>
                  Clear filter
                </Button>
              }
            >
              That team's provider accounts are not visible to this account — assignments are only
              readable by members of the owning team.
            </AlertBanner>
          ) : (
            <ErrorNote error={connections.error} onRetry={() => void connections.refetch()} />
          )}
        </div>
      ) : null}

      {!connections.isLoading && accounts.length === 0 ? (
        teamFilter ? (
          <EmptyTeamProviders
            teamLabel={teamName(teamFilter) ?? "this team"}
            onShowAll={() => setTeamFilter("")}
            onAdd={isAdmin ? openAdd : null}
          />
        ) : (
          <EmptyProviders onAdd={isAdmin ? openAdd : null} />
        )
      ) : (
        <>
          {!connections.isLoading && accounts.length > 0 ? (
            <ProvidersKpiRow
              summary={summary}
              connectedProviders={providerCounts.connected}
              healthyProviders={providerCounts.healthy}
              attentionProviders={providerCounts.attention}
              disabledProviders={providerCounts.disabled}
              filter={filter}
              onFilter={setFilter}
            />
          ) : null}
          <ProviderInventory
            groups={filteredGroups}
            filter={filter}
            onFilterChange={setFilter}
            quotas={quotas.data}
            subscriptions={subscriptions.data}
            quotaLoaded={quotaLoaded}
            trafficById={trafficById}
            providers={providers.data}
            oauth={oauth.data}
            selectedId={inspected?.id}
            loading={connections.isLoading}
            isAdmin={isAdmin}
            testingId={testingId}
            testsById={testsById}
            inspectHrefFor={inspectHrefFor}
            displayOf={displayOf}
            teamName={teamName}
            onConfigure={openConfigure}
            onAdd={(provider) => {
              // The add flow reads `?add=` on its own, so the card only has to write it.
              const next = new URLSearchParams(params);
              next.delete("inspect");
              next.set("add", provider);
              setParams(next, { replace: false });
            }}
            onTest={(id) => test.mutate(id)}
            onToggle={requestToggle}
            onRemove={(c) => {
              remove.reset();
              setRemoving(c);
            }}
            onOpenCodes={() => setCodesOpen(true)}
          />
        </>
      )}

      <AddProviderFlow
        open={adding}
        initialCustomProtocol={initialCustomProtocol}
        initialProvider={initialProvider}
        onClose={() => {
          setAdding(false);
          closeConfigure();
        }}
        onChooseOAuth={(provider) => {
          setSignIn(provider);
          setAdding(false);
        }}
        subscriptionProviders={subscriptionProviders}
        keyProviders={keyProviders}
        providers={providers.data ?? []}
        onAccountCreated={handleAccountCreated}
        onTestRecorded={recordTest}
      />

      {inspected ? (
        <ConnectionDrawer
          key={inspected.id}
          connection={inspected}
          spec={providers.data?.find((p) => p.id === inspected.provider)}
          display={displayOf(inspected.provider)}
          quota={quotaById.get(inspected.id)}
          subscription={subscriptionById.get(inspected.id)}
          quotaLoaded={quotaLoaded}
          teams={teamList}
          isAdmin={isAdmin}
          testing={testingId === inspected.id}
          toggling={toggle.isPending && toggle.variables?.id === inspected.id}
          lastTest={testsById[inspected.id] ?? null}
          onTest={() => test.mutate(inspected.id)}
          onToggle={() => requestToggle(inspected)}
          onRemove={() => {
            remove.reset();
            setRemoving(inspected);
          }}
          onClose={closeConfigure}
        />
      ) : null}

      {signIn ? (
        <OAuthDialog
          provider={signIn}
          onClose={() => setSignIn(null)}
          onConnected={handleOAuthConnected}
          onViewProviders={() => {
            setSignIn(null);
            window.requestAnimationFrame(() =>
              document.getElementById("providers-connected")?.scrollIntoView({ block: "start" }),
            );
          }}
        />
      ) : null}
    </>
  );
}
