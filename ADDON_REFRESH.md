# Optional addon renewal

BitChord does not ship an addon directory, add providers automatically, or guess a
provider from its hostname. The listener finds and adds each addon themselves.
Automatic renewal is off by default, including for sources saved by older builds.

In an addon's editor, enable **Automatically renew temporary URLs** and enter a
**Renewal link from the addon provider**. The Test button tests those settings
before saving. The original addon address and its configuration path are retained.
The renewal link and issued leases are stored in the secret tier on Apple devices.

## Supported renewal links

A renewal link may answer a GET with JSON containing an absolute HTTPS
`manifestUrl`, `addonUrl`, or `baseUrl`:

```json
{
  "manifestUrl": "https://catalogue.example/u/opaque.configuration/manifest.json",
  "expiresIn": 172800
}
```

`expiresAt` can instead be Unix seconds, Unix milliseconds, or an ISO-8601 string.
BitChord uses the advertised lifetime, including short lifetimes, and renews
shortly before expiry when the addon is used. With no advertised expiry it reuses
the lease until it is rejected. No fixed 48-hour schedule is assumed.

The addon protocol currently uses path-based configuration URLs. Returned addon
URLs containing user information, fragments, or query parameters are rejected;
the renewal link itself may have provider-issued query parameters.

A service using a SHA-256 prefix challenge may expose a JSON descriptor instead:

```json
{
  "challengeUrl": "/api/puzzle",
  "refreshUrl": "/api/issue"
}
```

The challenge GET returns `challenge` and `difficulty` (one to five leading zero
hex digits). BitChord finds a decimal `nonce` for SHA-256(`challenge` + `nonce`),
then POSTs those two fields as JSON to the issuing endpoint. That response uses
the URL/lifetime format above. Both endpoints must stay on the renewal link's
HTTPS origin. Renewal documents are limited to 16 KiB, redirects are rejected,
and the complete renewal has a 40-second deadline. Challenge work is separately
bounded, cancellable, and does not execute server code.

A provider's HTML renewal page is also supported when it explicitly contains
literal GET and POST `fetch` endpoints and a `SHA-256` declaration for that same
flow. BitChord reads those declarations; it never runs the page's JavaScript.
Other JavaScript flows, account sign-in, CAPTCHAs and unsupported APIs require
manual renewal or a provider-compatible endpoint. This is a supported protocol
set, not a claim that every website can be refreshed automatically.

## Runtime behavior

Requests for the same addon/renewal link share one renewal, including across
health checks and playback. A rejected token gets one renewal retry after HTTP
401, 403 or 410; a missing manifest may also be retried once. A track's ordinary
404 does not trigger renewal. A still-valid lease can keep playing through a
temporary renewal-service failure, but a rejected or expired lease cannot.
Cached manifest, search and stream answers are scoped to each issued lease, so
renewal also discards stale answers when the provider returns the same base URL.

Renewal is performed on demand. It does not require an iOS background timer or
keep the app running while the listener is not using it. Failed renewals do not
overwrite the configured source or discard other addons.
