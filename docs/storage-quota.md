# Hub storage quota research

- `GET /api/whoami-v2` identifies the token's owner with `name` and `isPro`.
  Its `orgs` entries have `name`, optional `plan`, `roleInOrg`, and
  `securityRestrictions`. It supplies plan and identity information without
  effective storage allowances, usage, or billed seat counts.
- `GET /api/settings/repositories` covers the authenticated user.
  `GET /api/organizations/{name}/settings/repositories` covers that organization.
  Both return arrays with `id`, `type`, `visibility`, and `storage` in bytes.
  The Python SDK's `list_user_repos` follows the `Link` header's `next`
  relation and includes buckets as well as Git-backed repos. Individual
  repo info uses `usedStorage`; the settings lists use `storage`.
- `GET /api/settings/billing/usage-v2` and
  `GET /api/organizations/{name}/billing/usage-v2` take integer
  `startDate` and `endDate` parameters for period usage. The published schema
  supplies no response contract for current storage usage and effective
  limits per visibility. All four candidate routes (including the user
  storage listing and whoami) returned HTTP 401 without credentials.
  Authenticated billing responses could not be verified in this environment.
- Repo revision info (`GET /api/{models|datasets|spaces}/{namespace}/{repo}/revision/{rev}`
  with `expand=private`) and bucket details
  (`GET /api/buckets/{namespace}/{bucket}`) expose the `private` flag. Repo info
  also resolves aliases to their canonical IDs before namespace selection.

Sources include the [Hub OpenAPI schema](https://huggingface.co/.well-known/openapi.json),
[endpoint reference](https://huggingface.co/.well-known/openapi.md),
[Hugging Face Python SDK](https://github.com/huggingface/huggingface_hub/blob/main/src/huggingface_hub/hf_api.py)
and [storage plans](https://huggingface.co/docs/hub/en/storage-limits).

Example storage-list response using synthetic data

```json
[
  {"id":"alice/data","type":"bucket","updatedAt":"2026-01-01T00:00:00Z","visibility":"private","storage":43500000000,"storagePercent":92.6},
  {"id":"alice/demo","type":"model","updatedAt":"2026-01-01T00:00:00Z","visibility":"public","storage":3500000000,"storagePercent":7.4}
]
```

For an authenticated free user named `alice`, a private mount in this example
reports approximately 100 GB total and 56.5 GB free. hf-mount marks this as
`HubEstimate`. It sums private entries in the mount's namespace across all
pages and uses the included allowance of 100 GB for free accounts or 1 TB
for personal PRO accounts. Above that allowance, paid private storage may
still be available through pay-as-you-go. A free organization requires an
unrestricted org admin and its own storage listing. Paid organization plans
require the billed seat count. The documented public allowances depend on
best-effort policies, grants, and add-ons. `storagePercent` is a listing metric
with no documented contract for deriving a scope's effective quota.

Public, anonymous, ambiguous, and unauthorized mounts use a 42 PiB
placeholder, as do requests that fail or exceed the four-second refresh
budget. Quota lookup stays independent of the Hub requests needed to mount
and access remote files. Resource-scoped tokens and non-admin organization roles can expose
partial usage. Protected Space storage has no documented billing scope. The
placeholder applies if those cases prevent a reliable estimate. WinFsp and
FUSE report the same cached bytes, with FUSE rounding down to whole blocks.
