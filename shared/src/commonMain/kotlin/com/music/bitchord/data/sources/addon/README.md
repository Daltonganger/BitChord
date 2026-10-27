# Addon session renewal

`AddonClient` delegates temporary endpoint renewal to `AddonSessions`. Ordinary
addons pass through unchanged. The registry currently contains one adapter,
`LastBabeSessionProvider`; adding another protocol requires an adapter rather
than changes to the client or Sources UI.

`AddonSession` owns secure lease persistence, expiry checks (ten minutes early),
serialized renewal, cancellation, a 40-second renewal deadline, and a 30-second
failure cooldown. A rejected token can trigger one renewal and request retry;
concurrent rejections of the old token reuse the fresh lease. Stored Last Babe
leases retain their existing secret key and JSON format.

Each `AddonSessionProvider` must define:

- A distinct secret-storage key.
- The exact configured origins and paths it supports.
- Which returned session URLs it accepts.
- Its renewal handshake, returning an endpoint and expiry time.

Do not infer a handshake from arbitrary addon URLs. The existing manifest does
not define a common authentication or renewal protocol. Last Babe's challenge,
proof-of-work and mint endpoints therefore remain in its named adapter.

Renewal transport uses the existing shared HTTP client, rejects redirects, reads
at most 16 KiB plus one overflow-detection byte, and cancels the remaining body.
Each request has a 15-second deadline. Provider responses, signed URLs and storage
exceptions are not propagated in renewal errors. Last Babe additionally checks
its HTTPS origin and token-path format, and bounds proof-of-work by iterations,
elapsed time and coroutine cancellation.

Coverage lives in `AddonSessionTest`, `AddonSessionTransportTest`, and
`LastBabeSessionTest`. They cover independent adapters, concurrent renewal,
persistence, failure cooldown, timeout, cancellation, response limits, rejected
tokens, invalid origins and ordinary-addon pass-through. `SourceQualityRouteTest`
separately verifies that source-quality preferences do not misrepresent Bluetooth
output capabilities.
