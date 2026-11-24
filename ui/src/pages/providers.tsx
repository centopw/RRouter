import { useCallback, useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { OAuthDialog } from "../components/oauth-dialog";
import {
  Button,
  ConfirmDialog,
  ErrorNote,
  PageHeader,
  Select,
  cx,
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
} from "../lib/queries";
import { ProviderInventory } from "./providers/account-row";
import {
  AddProviderFlow,
  EmptyProviders,
  EmptyTeamProviders,
  ProviderCodesModal,
} from "./providers/add-provider-flow";
import { ConnectionDrawer } from "./providers/connection-drawer";
import {
  buildProviderGroups,
  useConnectionMutations,
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
  // Both limit sources answered: only then may an account without bars read "no cap set".
  const quotaLoaded = isAdmin && quotas.isSuccess && subscriptions.isSuccess;

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

  const inspectHrefFor = useCallback(
    (connectionId: string) => {
      const next = new URLSearchParams(params);
      next.set("inspect", connectionId);
      return `?${next.toString()}`;
    },
    [params],
  );

  const closeInspect = useCallback(() => {
    if (!params.has("inspect")) return;
    const next = new URLSearchParams(params);
    next.delete("inspect");
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

  // `?add=<provider-id>` opens the add flow straight on that provider's key form.
  useEffect(() => {
    const requested = params.get("add");
    if (!requested || requested === "custom") return;
    if (!providers.data) return;
    const found = providers.data.find((provider) => provider.id === requested);
    if (isAdmin && found && found.kind !== "oauth") {
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
  }, [params, setParams, isAdmin, providers.data]);

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
        subtitle="Upstream accounts this gateway can route through."
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
        <p className="mb-3 text-small text-muted">
          Read-only: these are the accounts your team can route through. An administrator manages
          credentials and limits.
        </p>
      ) : null}
      {teamList.length > 0 ? (
        <p className="mb-4 text-small text-muted">
          Team keys route only to their own team's accounts; shared accounts serve instance-wide
          keys.
        </p>
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
        <div
          className={cx(
            "mb-3 flex items-start gap-2 rounded-lg border px-3 py-2 text-body tnum",
            notice.tone === "pos"
              ? "border-[var(--color-pos-line)] bg-[var(--color-pos-tint)] text-[var(--color-pos-ink)]"
              : "border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] text-[var(--color-neg-ink)]",
          )}
        >
          <span className="min-w-0 flex-1 break-words">{notice.text}</span>
          <button
            type="button"
            onClick={() => setNotice(null)}
            className="text-caption underline underline-offset-2"
          >
            dismiss
          </button>
        </div>
      ) : null}

      {connections.isError ? (
        <div className="mb-4">
          {apiCode(connections.error) === "team_forbidden" ? (
            <div
              role="alert"
              className="flex items-start gap-2 rounded-lg border border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-3 py-2 text-body text-[var(--color-warn-ink)]"
            >
              <span className="min-w-0 flex-1">
                That team's provider accounts are not visible to this account — assignments are only
                readable by members of the owning team.
              </span>
              <button
                type="button"
                onClick={() => setTeamFilter("")}
                className="shrink-0 text-caption underline underline-offset-2"
              >
                clear filter
              </button>
            </div>
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
        <ProviderInventory
          groups={groups}
          quotas={quotas.data}
          subscriptions={subscriptions.data}
          quotaLoaded={quotaLoaded}
          providers={providers.data}
          selectedId={inspected?.id}
          loading={connections.isLoading}
          isAdmin={isAdmin}
          testingId={testingId}
          testsById={testsById}
          inspectHrefFor={inspectHrefFor}
          displayOf={displayOf}
          teamName={teamName}
          onTest={(id) => test.mutate(id)}
          onToggle={requestToggle}
          onRemove={(c) => {
            remove.reset();
            setRemoving(c);
          }}
          onOpenCodes={() => setCodesOpen(true)}
        />
      )}

      <AddProviderFlow
        open={adding}
        initialCustomProtocol={initialCustomProtocol}
        initialProvider={initialProvider}
        onClose={() => setAdding(false)}
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
          onClose={closeInspect}
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
