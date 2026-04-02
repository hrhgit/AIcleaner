# Scan Rule Sources

This file records the official sources used to seed the first-pass static scan rules.

## Included In `safe_to_delete`

- Windows temporary files and Storage Sense guidance
  - Source: https://support.microsoft.com/en-us/windows/manage-drive-space-with-storage-sense-654f6ada-7bfc-45e5-966b-e24aded96ad5
  - Source: https://learn.microsoft.com/en-us/windows/configuration/storage/storage-sense
  - Applied to: `C:\Windows\Temp`, `%LocalAppData%\Temp`

- npm cache management
  - Source: https://docs.npmjs.com/cli/v7/commands/npm-cache/
  - Applied to: `%LocalAppData%\npm-cache`

- pip cache management
  - Source: https://pip.pypa.io/en/stable/topics/caching.html
  - Source: https://pip.pypa.io/en/stable/cli/pip_cache.html
  - Applied to: `%LocalAppData%\pip\Cache`

- Build caches with official framework/tool references
  - Source: https://nextjs.org/docs/14/pages/building-your-application/deploying/ci-build-caching
  - Source: https://v2.nuxt.com/docs/2.x/configuration-glossary/configuration-builddir/
  - Source: https://webpack.js.org/configuration/cache/
  - Applied to: `...\.next\cache`, `...\.nuxt`, `...\node_modules\.cache`

- Chrome and Edge browser caches using known Windows implementation paths
  - Evidence level: implementation-path allowlist, not a stable official Windows path contract
  - Applied to:
    - `%LocalAppData%\Google\Chrome\User Data\*\Cache`
    - `%LocalAppData%\Google\Chrome\User Data\*\Code Cache`
    - `%LocalAppData%\Microsoft\Edge\User Data\*\Cache`
    - `%LocalAppData%\Microsoft\Edge\User Data\*\Code Cache`

## Included In `keep`

- Windows system and application roots
  - Source basis: Microsoft Storage Sense guidance avoids broad deletion outside managed temporary files.
  - Applied to: `C:\Windows`, `C:\Program Files`, `C:\Program Files (x86)`, `C:\ProgramData`

- User content roots
  - Source basis: Storage Sense guidance distinguishes temporary files from user-content libraries.
  - Applied to: per-user `Desktop`, `Documents`, `Pictures`, `Videos`, `Music`

## Explicitly Excluded From v1 Static Rules

- Generic `Cache`/`Temp` names without a known safe path family
- `Downloads`
- Generic log folders
- Generic archive folders
- Firefox, Brave, Opera, and generic Chromium-derived browser cache paths
