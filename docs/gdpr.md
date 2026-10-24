# Data protection papers

The review service holds personal data of the client's reviewers. This is what the GDPR asks
the operator (the controller) to have in writing, filled in for the service as built. Replace
the bracketed parts with your own details.

## Article 30 record of processing

| | |
|---|---|
| Controller | [Your company], [address], [contact e-mail] |
| Purpose | Letting a client's team review page layouts in the browser: read, comment, suggest edits, decide on a round |
| Legal basis | Performance of the contract with the client (Art. 6(1)(b)); for reviewers the client invites, the client's legitimate interest in getting the work reviewed (Art. 6(1)(f)) |
| Data subjects | The client's reviewers; the operator's own staff who sign in as owners |
| Categories of data | Name, e-mail address, role; comments, replies, suggested text, drawings and decisions; sign-in and session times; the IP address and time of every change (audit log) |
| Recipients | Everyone with access to the project: its members and, through a public link, anyone with the link (and its password) |
| Sub-processors | See below |
| Transfers outside the EU | None by the service itself. With Turnstile configured, Cloudflare receives the visitor's IP and browser details for the challenge (Cloudflare's DPA and EU-US Data Privacy Framework certification cover it) |
| Retention | Sessions 12 hours (60 minutes idle); sign-in links 15 minutes; a project and everything in it `RETENTION_DAYS` (90) days after it is closed; a removed member goes at once unless comments cite them, then with the project |
| Security measures | Tokens hashed at rest, TLS by Caddy, cookie flags and origin checks, CSP, rate limits, read-only container, encrypted replica in EU object storage, audit log; README "Security notes" has the full list |

## Sub-processors

| Who | What | Where | Agreement |
|---|---|---|---|
| The hosting provider of the VPS (Hetzner Online GmbH in the EU setup; a US provider such as RackNerd LLC means reviewers' data leaves the EU and the record needs a transfer basis: the provider's DPA with standard contractual clauses, or its Data Privacy Framework listing) | Server | Where the VPS is | The provider's DPA, signed or accepted in its portal |
| The object-storage provider (Hetzner Object Storage, Cloudflare R2, Backblaze B2 or AWS S3) | Litestream replica and nightly backups | The bucket's region | The provider's DPA |
| Scaleway SAS, or Amazon Web Services EMEA SARL | Outgoing mail (magic links, invites, reminders, mentions, digests) | Paris, FR, or eu-central-1 (Frankfurt), DE | Scaleway: DPA in the console; AWS: DPA accepted in the account (Artifact) |
| Cloudflare, Inc. (optional) | Turnstile challenge on sign-in and join forms | Global edge; data under the EU-US DPF | DPA at cloudflare.com/cloudflare-customer-dpa |

Keep this list in the client contract's annex, and tell clients when it changes.

## Reviewers' rights

- Access and portability: `GET /api/projects/{id}/comments.json` holds everything a reviewer
  wrote; `GET /api/projects/{id}` their member record and audit events.
- Rectification: a reviewer edits their own comments; the owner updates a member's name through
  the API.
- Erasure: `DELETE /api/projects/{id}/members/{member}` removes a member (at once when nothing
  cites them); closing the project (`PATCH … {closed: true}`) removes everything after the
  retention period, or sooner with `RETENTION_DAYS` lowered for a day.
- Objection and complaints: the contact e-mail above; the supervisory authority of the
  controller's country.

## Breach plan

1. Rotate `ADMIN_TOKEN` and revoke every share link (`DELETE /api/share-links/{id}`); sessions
   die with the links' projects or by deleting the `session` rows.
2. Read the audit log (`event` table) for what was touched and from where.
3. Notify the supervisory authority within 72 hours when the breach is likely to risk the
   reviewers' rights (Art. 33), and the reviewers themselves when the risk is high (Art. 34).
4. Write it up: what, when, who was told, what changed.

## Not covered here

Your own privacy notice to reviewers (link it from the client's invite mail), the client
contract's data-processing clause (you process the client's reviewers' data on the client's
behalf as well as your own), and the cookie notice: the service sets one strictly necessary
session cookie and nothing else, which needs no consent banner.
