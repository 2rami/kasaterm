# KasaViewer build and delivery

`scripts/build-viewer-app.sh` builds `dist/KasaViewer.app` from the workspace version. It does not install or restart either desktop app. Use a clean worktree for the intended release version; `CARGO_TARGET_DIR` can point at its existing build cache.

```sh
KASATERM_SIGN_ID='<Developer ID certificate fingerprint>' \
KASATERM_SIGN_KEYCHAIN='<existing signing keychain path>' \
KASATERM_SIGN_HARDENED=1 \
bash scripts/build-viewer-app.sh --verify-open
```

The explicit identity and keychain are passed to `codesign`. A missing identity, signing failure, or failed signature verification stops the build; it never falls back to ad-hoc signing when an identity was requested. Hardened signing requires a Developer ID Application certificate, enables the runtime and secure timestamp, and uses the existing application entitlements. If the caller provides `KASATERM_SIGN_UNLOCK`, that executable runs immediately before hardened signing. No signing credentials are read by the script. Without an explicit identity, ordinary development builds retain the `kasaterm-dev` or ad-hoc behavior.

`--verify-open` exercises the production macOS document handler in a separate test bundle: opening a file while launching, opening more files in a running process, and paths containing Korean characters and spaces. Finder sends an Apple Event, whereas CLI file arguments enter through `ViewerLaunch`; testing only CLI arguments does not cover the Finder launch path.

KasaViewer does not currently receive Sparkle updates directly. Its bundle contains neither Sparkle nor a feed URL, and viewer startup does not initialize the main app updater. The main kasaterm automatic update channel does not deliver KasaViewer. Build and verify the viewer separately, then replace its installed bundle after the viewer has exited normally; an active document window must not be replaced or force-quit by the build script. Signing alone does not constitute notarization.

The one-time `tools.release.bootstrap` stage/arm/status workflow also accepts KasaViewer. It pins the selected machine, signed bundle identity, version and executable hash, waits for that exact viewer executable, and atomically exchanges bundles while retaining the old one. It does not enable the main application's preview channel or wait for unrelated Kasaterm windows. See [receiver setup](automatic-preview-updates.md#처음-한-번-받는-mac).
