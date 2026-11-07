# Community providers

Reviewed API-key providers live here as one JSON manifest per provider. The auth crate
compiles every manifest into the next binary through `crates/rrouter-auth/build.rs`; no
runtime download or arbitrary code execution is involved.

## Add one

1. Open **Providers → add provider → API key → “Missing your favorite provider?”** and
   choose **API provider**. The issue form asks for the endpoints, auth style, model list,
   capabilities and documentation.
2. The `provider-api` issue label triggers `.github/workflows/provider-request.yml`, which
   runs `scripts/provider-from-issue.mjs` and opens a pull request adding
   `providers/community/<id>.json`.
3. A maintainer checks the manifest against the provider's public documentation. Once
   merged, the next release can route to it.

Never include an API key, OAuth token, client secret or private endpoint in an issue or a
manifest. OAuth providers are handled manually because they need a reviewed client flow;
use the **OAuth provider** issue form.

## Manifest

See [`../schema.json`](../schema.json). A minimal manifest:

```json
{
  "id": "acme",
  "display": "Acme AI",
  "base_url": "https://api.acme.ai/v1/chat/completions",
  "auth": "bearer",
  "default_dialect": "chat",
  "models_url": "https://api.acme.ai/v1/models"
}
```
