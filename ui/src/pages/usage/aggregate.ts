/**
 * Pure rollups behind the Usage page. One source feeds everything:
 * `/api/usage/summary?group_by=connection`. Account rows group it by connection, and
 * provider/model rows fold the same rows by (provider, model), which is exactly what
 * the ungrouped summary would return, so the two can never disagree.
 */

import type { UsageSummaryByConnection as ConnectionRow, UsageSummaryRow } from "../../lib/api";

export type Totals = {
  requests: number;
  errors: number;
  /** Input + output tokens, matching the Activity page. Cache tokens are excluded. */
  tokens: number;
  /** USD in micros, priced from catalog prices. */
  costMicros: number;
};

export type ModelNode = { model: string; totals: Totals };

export type ProviderNode = { provider: string; totals: Totals; models: ModelNode[] };

export type AccountNode = {
  /** `null` is the "Unattributed" bucket for requests no account was recorded for. */
  connectionId: string | null;
  totals: Totals;
  models: ModelNode[];
  /** False for a known connection the range has no requests for. */
  observed: boolean;
};

export type AccountRef = { id: string; provider: string };

export function emptyTotals(): Totals {
  return { requests: 0, errors: 0, tokens: 0, costMicros: 0 };
}

export function addTotals(target: Totals, source: Totals): Totals {
  target.requests += source.requests;
  target.errors += source.errors;
  target.tokens += source.tokens;
  target.costMicros += source.costMicros;
  return target;
}

function addSummaryRow(totals: Totals, row: UsageSummaryRow): void {
  totals.requests += row.requests;
  totals.errors += row.errors;
  totals.tokens += row.input_tokens + row.output_tokens;
  totals.costMicros += row.cost_micros;
}

/** Error rate as a percentage, 0 when nothing was served. */
export function errorRateOf(totals: Totals): number {
  return totals.requests > 0 ? (totals.errors / totals.requests) * 100 : 0;
}

function bySpend(a: { totals: Totals }, b: { totals: Totals }): number {
  return b.totals.costMicros - a.totals.costMicros || b.totals.tokens - a.totals.tokens;
}

function byTokens(a: ModelNode, b: ModelNode): number {
  return b.totals.tokens - a.totals.tokens || a.model.localeCompare(b.model);
}

/**
 * Summary rows → providers, ranked by spend then tokens, each with ranked models.
 * Connection-grouped rows fold to the same result: rows for one (provider, model)
 * across several accounts are summed into one model node.
 */
export function buildProviderNodes(rows: UsageSummaryRow[]): ProviderNode[] {
  const providers = new Map<string, Map<string, Totals>>();
  for (const row of rows) {
    let models = providers.get(row.provider);
    if (!models) {
      models = new Map();
      providers.set(row.provider, models);
    }
    let totals = models.get(row.model);
    if (!totals) {
      totals = emptyTotals();
      models.set(row.model, totals);
    }
    addSummaryRow(totals, row);
  }
  const nodes: ProviderNode[] = [];
  for (const [provider, models] of providers) {
    const totals = emptyTotals();
    const modelNodes: ModelNode[] = [];
    for (const [model, modelTotals] of models) {
      addTotals(totals, modelTotals);
      modelNodes.push({ model, totals: modelTotals });
    }
    modelNodes.sort(byTokens);
    nodes.push({ provider, totals, models: modelNodes });
  }
  nodes.sort((a, b) => bySpend(a, b) || a.provider.localeCompare(b.provider));
  return nodes;
}

/** Connection-grouped summary rows → per-provider account rows with their models. */
export function buildAccountNodes(rows: ConnectionRow[]): Map<string, AccountNode[]> {
  const byProvider = new Map<string, Map<string, AccountNode>>();
  for (const row of rows) {
    let accounts = byProvider.get(row.provider);
    if (!accounts) {
      accounts = new Map();
      byProvider.set(row.provider, accounts);
    }
    const key = row.connection_id ?? "";
    let account = accounts.get(key);
    if (!account) {
      account = {
        connectionId: row.connection_id,
        totals: emptyTotals(),
        models: [],
        observed: true,
      };
      accounts.set(key, account);
    }
    let model = account.models.find((node) => node.model === row.model);
    if (!model) {
      model = { model: row.model, totals: emptyTotals() };
      account.models.push(model);
    }
    addSummaryRow(model.totals, row);
    addSummaryRow(account.totals, row);
  }
  const out = new Map<string, AccountNode[]>();
  for (const [provider, accounts] of byProvider) {
    const nodes = [...accounts.values()];
    for (const node of nodes) node.models.sort(byTokens);
    nodes.sort(bySpend);
    out.set(provider, nodes);
  }
  return out;
}

/**
 * Add every known connection as an account row, even when the range has no requests
 * for it, so a configured-but-idle account still shows its quota.
 */
export function mergeKnownAccounts(
  observed: Map<string, AccountNode[]>,
  known: AccountRef[],
): Map<string, AccountNode[]> {
  const out = new Map<string, AccountNode[]>();
  for (const [provider, nodes] of observed) out.set(provider, [...nodes]);
  for (const ref of known) {
    let nodes = out.get(ref.provider);
    if (!nodes) {
      nodes = [];
      out.set(ref.provider, nodes);
    }
    if (nodes.some((node) => node.connectionId === ref.id)) continue;
    nodes.push({
      connectionId: ref.id,
      totals: emptyTotals(),
      models: [],
      observed: false,
    });
  }
  for (const nodes of out.values()) {
    nodes.sort(
      (a, b) => Number(b.observed) - Number(a.observed) || bySpend(a, b),
    );
  }
  return out;
}

/**
 * Append an empty provider row for each provider in `withQuota` that has no traffic
 * in the range. Quota is current-window, not range-gated, so a provider idle in the
 * last hour must not take its 95%-used plan out of the table with it.
 */
export function withQuotaShells(nodes: ProviderNode[], withQuota: Iterable<string>): ProviderNode[] {
  const present = new Set(nodes.map((node) => node.provider));
  const shells: ProviderNode[] = [];
  for (const provider of withQuota) {
    if (present.has(provider)) continue;
    present.add(provider);
    shells.push({ provider, totals: emptyTotals(), models: [] });
  }
  if (shells.length === 0) return nodes;
  shells.sort((a, b) => a.provider.localeCompare(b.provider));
  return [...nodes, ...shells];
}
