# Addon renewal

Renewal is optional per addon and uses a renewal link supplied by the listener.
No provider, hostname, addon directory or automatic provider adapter is bundled.
See [the renewal protocol documentation](../../../../../../../../../../ADDON_REFRESH.md)
for configuration, supported response formats and lifecycle behavior.

`AddonRefreshSession` owns each configured link's serialized renewal and cached
lease. `AddonSessionTransport` bounds documents to 16 KiB, applies request
timeouts, and prevents redirects before any renewal response is processed. The
complete renewal has a 40-second deadline; challenge computation is separately
bounded and cancellable. Remote JavaScript is never executed.
