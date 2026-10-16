# Shared local/internal signing policy. This file never creates keys or changes trust.
veyqa_configure_signing() {
  local config="${VEYQA_SIGNING_CONFIG:-$HOME/Library/Application Support/Veyqa Build/signing-identity.sha1}"
  if (( ! ${+VEYQA_SIGNING_IDENTITY} )); then
    if [[ -e "$config" ]]; then
      [[ -r "$config" ]] || { print -u2 "Cannot read signing configuration: $config"; return 1; }
      VEYQA_SIGNING_IDENTITY="$(<"$config")"
    else
      VEYQA_SIGNING_IDENTITY="-"
    fi
  fi
  if [[ "$VEYQA_SIGNING_IDENTITY" != "-" ]]; then
    if [[ ! "$VEYQA_SIGNING_IDENTITY" =~ '^[[:xdigit:]]{40}$' ]]; then
      print -u2 "Signing identity must be an exact certificate SHA-1 fingerprint."
      return 1
    fi
    VEYQA_SIGNING_IDENTITY="${(U)VEYQA_SIGNING_IDENTITY}"
    if ! security find-identity -v -p codesigning | awk -v wanted="$VEYQA_SIGNING_IDENTITY" \
      '$2 == wanted { found=1 } END { exit !found }'; then
      print -u2 "Configured signing identity is unavailable. Refusing ad-hoc fallback."
      return 1
    fi
    VEYQA_SIGNING_PROFILE="local"
  else
    VEYQA_SIGNING_PROFILE="internal"
  fi
  export VEYQA_SIGNING_IDENTITY VEYQA_SIGNING_PROFILE
}

veyqa_sign() {
  codesign --force --sign "$VEYQA_SIGNING_IDENTITY" --timestamp=none "$@"
}

veyqa_sign_app() {
  local app_path="$1"
  local entitlements="$2"
  [[ -d "$app_path/Contents" && "$app_path" == *.app ]] || return 1
  local item
  # Sign actual Mach-O code only, then seal nested bundles from inside out.
  # No --deep signing, no trust overrides and no weakening of system policies.
  while IFS= read -r -d '' item; do
    if [[ "$(/usr/bin/file -b "$item")" == *Mach-O* ]]; then
      veyqa_sign "$item" || return 1
    fi
  done < <(find "$app_path/Contents" -type f -print0)
  while IFS= read -r -d '' item; do
    veyqa_sign "$item" || return 1
  done < <(find "$app_path/Contents" -depth -type d \( -name '*.framework' -o -name '*.app' \) -print0)
  veyqa_sign --entitlements "$entitlements" "$app_path" || return 1
  codesign --verify --deep --strict --verbose=2 "$app_path"
}
