# Google and GitHub account login

OAuth is disabled until the relay administrator registers provider applications and supplies server-only configuration. No provider client secret or provider access/refresh token is stored in a desktop app, mobile app, account settings, or the identity database.

## Registration

1. In [Google Auth Platform](https://console.cloud.google.com/auth/clients), create a **Web application** OAuth client. Configure the consent screen and test users as required by the project. Set the exact redirect URI to `https://YOUR-RELAY/relay/oauth/google/callback`.
2. In [GitHub Developer settings](https://github.com/settings/applications/new), create an **OAuth App**. Use the relay origin as the homepage and set the exact callback URL to `https://YOUR-RELAY/relay/oauth/github/callback`.
3. Configure these variables only in the private relay service environment:

   - `KASA_OAUTH_PUBLIC_ORIGIN`: one fixed HTTPS origin, without a path, query, credentials, or custom port.
   - `KASA_OAUTH_GOOGLE_CLIENT_ID` and `KASA_OAUTH_GOOGLE_CLIENT_SECRET`.
   - `KASA_OAUTH_GITHUB_CLIENT_ID` and `KASA_OAUTH_GITHUB_CLIENT_SECRET`.

   With the default KASA gateway, the origin is `https://kasaterm.debimarlene.com`; both registration callbacks must use that same origin. Each provider is independently disabled if its configuration is missing or malformed.
4. Deploy the tested relay under the normal deployment procedure. Merely rebuilding the desktop app does not enable server login. Check `GET /relay/oauth/providers` for enabled providers without exposing secret values.
5. Initially sign in with the existing KASA account/password, then choose **Google 연결** or **GitHub 연결** in device-account settings. Every browser flow names the requesting device and asks for the verification code displayed only in that KASA app. Linking also names the destination KASA account. Future devices can use that provider's login button, with the same device-verification step.

New OAuth-only accounts are **not** created by default. `KASA_OAUTH_ALLOW_SIGNUP=1` explicitly enables public sign-up for successfully verified provider identities. Enabling this is an administrator policy choice, not a build default. Otherwise an unlinked identity receives `account_not_linked`. Do not enable public registration merely to test the buttons.

## Identity and security contract

- Google identities use the verified Google issuer and `sub`; GitHub identities use GitHub's numeric user `id`. The provider namespace is part of the identity key. Email addresses and usernames are not identity keys and never trigger account merging.
- Linking requires an active KASA device credential at start and completion. The original account, device and credential hash remain bound to the request. Revocation, a conflicting existing link, disabled accounts, or another identity cannot transfer a link.
- GET only prepares a browser session and displays the requesting device; it never approves login. A same-origin POST must provide that session's CSRF token, secure cookie and the app-only verification code before the provider authorization URL is released. Five incorrect codes terminate the request. Device labels are untrusted and escaped. A forwarded login link alone is not sufficient to authorize a remote requester.
- Authorization-code flows use a 256-bit random state, an HttpOnly/Secure/SameSite=Lax browser cookie, PKCE S256, ten-minute expiry, and single-use callback/poll consumption. Google ID tokens additionally require RS256 verification against Google's fixed JWKS endpoint, issuer, audience, nonce, expiry, issued-at and verified email checks.
- Provider endpoints and callback origins are fixed; requests cannot supply alternate redirects, discovery URLs or user-info endpoints. HTTP clients refuse redirects and have bounded response sizes and deadlines. Errors never reflect provider token responses.
- Browser callbacks only show completion text. KASA device tokens are returned once to the separately authenticated poll capability, bound to provider, device kind and machine ID. All responses are `no-store`. Confirmation HTML uses `same-origin` referrer policy so its POST retains a valid Origin without sending its URL cross-origin; callbacks, redirects and JSON responses use `no-referrer`.
- The server stores provider identity/account mappings in `relay-oauth-identities.json` next to the gateway state, with private file permissions and atomic replacement. No provider credentials are written there. Existing password accounts, device records, settings sync and agent-account catalogs remain separate and supported. Unreadable identity storage disables OAuth instead of overwriting it.
- A desktop login attempt is bound to the local credential epoch and gateway. Logout, cancellation, a newer attempt or a changed account prevents an old response from saving over the new state. Credentials and poll capabilities are absent from render snapshots.
- OAuth token issuance never automatically revokes existing device credentials. Losing the poll response, failing to save the new credential, or rejecting a late OAuth result therefore cannot destroy a newer password login. Unused or older credentials remain visible through the existing device list and can be explicitly revoked. This does not change the legacy password-login replacement behavior.

## Relay administration page

`GET /relay/admin` lists who signed up and roughly how they use the relay. It is disabled (404) unless the private relay environment sets `KASA_RELAY_ADMINS` to a comma-separated list of relay account names. Request bodies, cookies and headers cannot designate an administrator.

- Sign-in reuses the provider flow: `POST /relay/admin/login/{provider}` (same-origin form) starts a request with a 256-bit state, a state cookie and PKCE S256, and the shared callback finishes it. The browser that starts the request also receives the result, so no app verification code is involved. The identity must already be linked to a listed account; this flow never creates an account or a device credential, and unknown identities receive the same refusal as non-administrators.
- A successful sign-in sets `__Secure-kasa_admin` (HttpOnly, Secure, SameSite=Lax, `Path=/relay/admin`, two hours). The relay keeps only its SHA-256 in memory, so a restart signs administrators out. Every request re-checks the listing and account state; disabling the account ends access immediately. `POST /relay/admin/logout` requires the same origin.
- Each row shows the account name, display name, sign-up route (`google`, `github` or `password`) and time, linked login methods, active desktop/phone device counts, current desktop connections, last access, and approximate usage. `GET /relay/admin/accounts` returns the same rows as JSON. Responses are `no-store`, framing is denied and forms may only post to the relay and the fixed providers.
- Usage counts uplink connection time, uplink frame bytes in both directions and relayed request count per account in `relay-usage.json` (private permissions, written every five minutes and on disconnect). Phone traffic through an account device is counted on that device. Request and response contents, including conversations and screens, are never stored or shown.
- To designate the operator: add `KASA_RELAY_ADMINS` to the relay service environment, reload the service, then link Google or GitHub to that account from the app before signing in.

## Display names

OAuth sign-up records the provider, sign-up time and a display name in the identity file: the verified Google email or the GitHub login. It is display-only and never an identity key or merge criterion. Signing in again with the sign-up provider refreshes it; a later linked provider does not rename the account. Password accounts keep their chosen name.

`/relay/whoami`, `/relay/devices`, the uplink welcome and the OAuth `complete` response carry `display_name`. The desktop account screen shows it instead of the `oauth_<hex>` account name.

## Client API

- `GET /relay/oauth/providers` returns public provider availability and whether new sign-up is allowed.
- `POST /relay/oauth/start` accepts `provider`, `kind` (`desktop` or `phone`), `machine_id`, `label`, and optional `link`. Linking also requires the existing device's bearer token. It returns `authorization_url`, `request_id`, `poll_token`, `user_code`, and `expires_in`.
- Display `user_code` in the requesting app and open `authorization_url` in the system browser, never an embedded credential form. The browser asks the user to enter that code, then POSTs it with its CSRF/session binding to the same URL. GET and incorrect/missing confirmation never redirect to the provider.
- `POST /relay/oauth/poll` and `/relay/oauth/cancel` accept `request_id`, `poll_token`, `provider`, `kind`, and `machine_id`. Poll returns `pending`, `linked`, or `complete`; only `complete` carries the KASA device credential.
- Desktop `relay.account` operations are `oauth_providers`, `oauth_start`, `oauth_poll`, and `oauth_cancel`. The start operation keeps the poll capability in process memory and exposes only `authorization_url`, `flow_id` and `user_code`. Poll/cancel require that same `flow_id`.

The server API supports a stable phone installation ID, but this change only wires desktop UI. Mobile provider buttons and physical-device validation remain separate work. A cold relay restart cancels pending browser attempts; durable identity links survive.

## Verification

Tests use a local provider mock and an ephemeral, locally generated RS256 fixture whose private key was discarded. They cover required POST device confirmation, wrong Origin/CSRF/cookie/code and code attempt limits; invalid/missing state and cookies, replay, expired/cancelled requests, provider/device/capability mismatches, revoked-device links, cross-account conflicts, provider-ID collisions, disabled sign-up, damaged storage, safe headers, redirect rejection, JWT signature/claim tampering, and local account/gateway races. Late OAuth completion, lost responses and both client/server save failures must preserve existing credentials. No real provider credentials are used by tests.

Primary references: [Google OpenID Connect](https://developers.google.com/identity/openid-connect/openid-connect), [GitHub OAuth authorization](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps).
